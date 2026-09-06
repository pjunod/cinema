# Validation — make cascading change impact explicit

Companion to [FEATURES.md](FEATURES.md) (what plurx does) and
[ARCHITECTURE.md](ARCHITECTURE.md) (how it is built) — this is how a behavior
becomes a named contract with an automatic test obligation.
[DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md) owns the separate question
of *when* a large effort pays for those obligations.

A functionality point is not a test file or a coverage percentage. It is one
user-visible promise, the paths capable of changing that promise, and the
checks that provide evidence for it. The catalog lives in
[`validation/points.toml`](../validation/points.toml); the runner is
[`scripts/validate`](../scripts/validate).

## Start here — the normal development loop

The validator is a coordinator around the checks the repository already has.
It works out which user-visible promises a change could affect, runs the
relevant checks, and records why each check ran.

```bash
make validate-help     # show the short version of this workflow
make validate-plan     # explain what the staged change selects; run nothing
make validate-staged   # validate the staged change before committing
make validate          # validate every point with the normal local profile
make validate-full     # include browser, client, and packaging checks
make validate-nightly  # exhaustive playback, recovery, bounds, and packaging
```

Use `make validate-staged` during ordinary work. Use `make validate` when the
working tree is complicated or path selection itself is in doubt. Use
`make validate-full` before a risky merge or release; it can take longer and
some checks need browsers, simulators, Docker, or client toolchains.

Task PRs into `effort/**` use `make effort-rust-check` plus affected client and
web compile/static checks instead of a validation profile. That lane is a
deliberate integration proof, not release evidence. The final `effort/**` to
`main` PR runs the full CI fan-out and writes an exact-tree qualification
record. A conflict-resolution branch named `integration/*-into-main` uses the
same qualification lane so resolving the merge does not discard that evidence.

## CI control plane — Forgejo is authoritative

Forgejo at `http://192.168.4.7:3000/noirr/plurx` owns repository events,
workflow state, logs, artifacts, packages, and runner assignment. GitHub
Actions is disabled. Forgejo pushes refs to `pjunod/plurx` as an external
mirror; GitHub does not send source, credentials, or jobs back into the local
pipeline.

The workflow files remain under `.github/workflows/` because Forgejo uses that
directory when `.forgejo/workflows/` is absent. There is deliberately one
workflow source rather than a local copy and a mirror copy that can drift.
First-party actions are qualified to `data.forgejo.org`; third-party actions
use explicit upstream URLs so resolution never depends on an instance default.

`make check` is the mandatory portable repository baseline: catalog lint,
historical regression coverage, operations source contracts, Rust formatting,
clippy, and the Rust test suite. Point-aware validation always includes the
catalog, history audit, and Rust gate, then adds checks for affected surfaces
that Rust cannot see, such as embedded JavaScript, browser structure, Android
code, or release packaging.

## The contract — promise, impact, evidence

Every point has six pieces:

| Field | Meaning |
|---|---|
| `id` | Stable name used in plans and reports, such as `playback.pipeline` |
| `title` | Short operator-facing name |
| `contract` | The behavior that must remain true |
| `paths` | Repository globs that can affect the behavior |
| `checks` | Commands that produce evidence for the contract |
| `depends_on` | Provider contracts whose changes can cascade into this consumer |

One file may affect several points. Impact walks **from provider to consumer**:
a change to the core media model selects the server and every client that
consumes it. A change confined to the embedded web player selects
`web.experience`, but does not rerun upstream database and server contracts as
though the browser supplied them. Shared checks run once even when several
selected points ask for the same gate.

```text
 changed paths
      │
      ▼
 direct point matches ──▶ downstream consumer closure
      │                         │
      └────────────┬────────────┘
                   ▼
       checks allowed by profile
                   │
                   ▼
        deduplicate ──▶ execute ──▶ JSON + JUnit + logs
```

Path selection never suppresses the baseline. `catalog-contract`,
`history-regressions`, and the Rust gate are `always_checks`, so every
commit-profile or CI-profile run still validates the catalog and history and
executes the workspace suite. Impact selection only adds checks; a bad path
mapping cannot quietly make ordinary tests disappear.

The Rust gate has two forms that must never drift apart in coverage, only in
packaging. Local profiles (`commit`, `full`, `nightly`) run `rust-gate` —
`make rust-check`, the one-command fmt + clippy + full-workspace suite. The
`ci` profile and the PR workflow run `rust-gate-ci` — `make ci-rust-gate` —
which owns formatting, Clippy, unit tests, and SQLite contracts in one job.
Four stable verdicts separately own replicated Store semantics, the topology
harness, WAL recovery, and real daemons. The Store verdict selects either the
complete legacy run or two binary-level shards according to
`CI_EXECUTION_MODE`; its required job name does not change during rollout.
Each shard discovers the compiled binary's current inventory, executes one
exact multi-filter invocation, and retains its assignment, outcomes, durations,
candidate tree, compiler identity, and binary digest. The aggregate accepts
only a disjoint exact union from byte-identical binaries. A weekly scheduled
job runs the complete unsharded inventory as an independent backstop. Store and
topology retain independent logs and exact-tree receipts, so a slow or failed
lane is attributable without re-running the other one. Excluding
`plurx-cluster-check` from the fast lane keeps Cargo's feature unification from
compiling those replicated contracts into it. The subset re-runs (`api-wire`,
`security-boundaries`, `user-journey`) stay out of the `ci` profile for the
same reason: there they would re-execute binaries the workspace run already
executed with identical feature resolution.

## Profiles — fast by default, deep when the environment can prove more

| Profile | Intended use | Additional evidence |
|---|---|---|
| `commit` | Pre-commit and ordinary local work | Mandatory Rust/catalog baseline; shared API wire check; web syntax, contrast, golden, and accessibility when affected |
| `ci` | Main-bound PRs and `main` | Ordinary PRs scope to the diff; `effort/**` and `integration/*-into-main` qualification PRs plus push events enable every surface. Task PRs into `effort/**` use the separate compile-only workflow |
| `full` | Before a risky merge or release | Browser playback; both native-client suites; Android device tests when an explicit disposable device is selected; container startup/restart |
| `nightly` | Scheduled deep regression search | Exhaustive playback and restart matrix; interrupted-production recovery; resource bounds; all runnable full checks; a gating 15-minute PGS parser fuzz campaign; report-only mutation sampling over Rust files changed in the last seven days |

The `commit` profile permits explicitly optional checks to skip when a laptop
lacks their tooling. The skip is printed and recorded; it is not reported as a
pass. `--strict` turns missing tools or files into failures for checks selected
on that platform. A platform mismatch remains a skip because Linux cannot run
XCTest, regardless of strictness.

CI fetches full Git history and selects from the pull-request base. The main
workflow runs mobile release hygiene when applicable, plus the history audit,
catalog and validation unit tests, and operations contracts. The effort
workflow keeps the latter three policy checks but defers mobile release
versioning until final qualification, so one large client project claims one
store build rather than a new build number for every internal task.

Mobile release hygiene reads two different refs, and the distinction is
load-bearing. `PLURX_VALIDATION_BASE` is the recorded pull-request base sha and
scopes *which* release inputs the branch touched; it is the branch point.
`PLURX_VALIDATION_MERGE_TARGET` is `origin/<base ref>` re-fetched when the job
runs, and supplies the counters the branch has to clear, because that is what
the branch actually merges into. They name the same commit only until the base
moves. Scope is the whole of scope: both the changed paths and the
workspace-version comparison that marks a release are read against the recorded
base, and only the two counters are read against the target. That split is
load-bearing in both directions. Reading the workspace comparison off the target
would conflate "this branch shipped a release" with "a release landed on the
target", so every branch open across a release — including an ordinary
dependency-only `Cargo.toml` edit, which is in this point's paths — would go red
and be told to bump two store counters it never touched. Reading the counters
off the branch point is the original defect. Two branches that bump a build
counter to the same value auto-merge with
no conflict marker and produce no `BEHIND` signal in this job, so a branch
measured only against its branch point stays green forever once an unrelated
release bump lands that same counter on the base. Re-baselining means a pull
request can go red without its own head moving; that is correct, and the
failure names the base ref and both counters so the fix is unambiguous without
reading the workflow. An unreadable merge target fails the check rather than
falling back to the branch point: scope selection fails open because a bad diff
base only costs time, but a missing counter baseline would report green on a
tree that cannot ship. A local `--changed-from` run passes one ref and uses it
for both roles, which is right for a branch measured against a fixed point.
Every expensive fan-out job in the main CI workflow waits for that preflight.
A documentation-only pull request stops after those executable documentation
contracts; it does not compile the Rust workspace or provision browsers and
native-client environments. A documentation change may include
`validation/regressions.d/**` and keep this lane because those entries are
consumed only by the history audit that preflight already runs; selector code and
`validation/points.toml` still take the executable-change lane.

For executable changes, the portable Rust and focused Linux contracts remain
one baseline job. Browser layout, Android JVM, Apple simulator, Android device,
release-build, and container checks run as parallel jobs only when the diff can
affect their contracts. Coverage runs after merge on `main`, where its badge is
published; a pull request does not rerun the Rust suite merely to discard the
number. [CI_TEST_OVERHAUL_PLAN.md](CI_TEST_OVERHAUL_PLAN.md) records the
measured failure history and the remaining suite-splitting, invalidation,
rebase-evidence, and telemetry milestones.

The impact graph selects browser and native unit suites. Explicit path owners
select the narrower environment checks: Android application and build files
justify an emulator; root Cargo manifests and the pinned toolchain justify
cross-target release builds; image, Compose, runtime configuration, and
lifecycle files justify the container smoke test. Ordinary `crates/**`
changes run the fast Rust lane, plus the four cluster verdicts when they touch a
cluster contract. CI routing changes deliberately run those same four Rust
lanes and the static workflow contracts, not every unrelated platform. Final
effort qualification, main pushes, tags, merge-group events when enabled, and
the nightly workflow retain the full fan-out before release.

One routing path is judged by content rather than by name. The replicated
Store lane costs 26-29 minutes, and forcing it on every `validation/points.toml`
edit charged that to diffs that could not have affected it: three of the four
pull requests that took a full `ci` run in a four-day sample selected the lane
by the routing rule alone rather than by touching cluster code. When
`validation/points.toml` is the **only** routing path a diff touches, the lane
is now forced only if the catalog edit could actually hide it — that is, if
between the base revision and the working tree the edit changed the `paths` or
`checks` list of `cluster.auth`, `cluster.membership`, or `cluster.page-reads`,
added or removed one of those points, or changed any field of the `cluster-auth`
check. Those are exactly the fields `resolve_scope` reads to select the lane, so
an edit that changes none of them cannot suppress cluster evidence. A `contract`
string is prose that documents a point rather than selecting it, and editing one
no longer buys a half-hour job. Removing a path from `cluster.auth` still runs
the lane, because that is precisely the suppression the routing rule exists to
catch.

Everything else about the rule is unchanged. The Rust gate stays forced for
every routing path, `validation/points.toml` included. A diff that also touches
`ci.yml`, `effort-ci.yml`, `lint.yml`, `validation/ci_scope.py`, or
`validation/runner.py` still forces the cluster lane unconditionally: a change
to the job graph or to selection itself is not reducible to a catalog
comparison. The comparison fails open in the same direction as the rest of
scope selection and more loudly — an unreadable base blob, a base or working
catalog that will not parse, an unexpected shape, or any other exception prints
one `ci-scope:` line to stderr and enables the lane. That asymmetry is
deliberate: a wasted half hour is recoverable, and silently dropped cluster
evidence is not.

`Main promotion gate` waits for every selected main-workflow job and accepts an
unselected job only when Forgejo records it as skipped. An effort qualification
is stricter: scope enables every surface and the qualification writer refuses
any result other than `success`. `Effort development gate` separately waits
for policy plus every affected compile/static job. Configure only those two
aggregates in Forgejo branch protection: `Main promotion gate` on
`main`, and `Effort development gate` on `effort/**`. Individual jobs remain
visible evidence but do not become permanent branch rules. Pushes and tags
enable all surfaces, and an absent or invalid
pull-request base also enables all jobs. Impact optimization therefore fails
open: a bad diff base costs time; it never suppresses tests. The scheduled
workflow still runs the `nightly` profile.

### Runner boundary — every workflow is local

Every `runs-on` declaration includes `self-hosted` and the exact lab capability
labels it needs. There is no hosted-runner switch or automatic fallback. A
missing local capability therefore leaves a visible queued job instead of
moving trusted repository code to an external execution boundary.

### The runner roster — a label is a claim about a machine

[`validation/runner-fleet.toml`](../validation/runner-fleet.toml) lists every
self-hosted runner registered to this repository, the physical host it sits on,
and the labels it carries. It mirrors `pjunod/ansible`'s
`github-runners/inventory/` plus the two hand-registered ARM64 laptop VMs, and
it must be updated in the same change that changes the fleet — a roster that
has drifted from reality proves nothing.

It exists because a `runs-on` naming a label nothing carries is not an error
Forgejo reports. The job queues, indefinitely, and is eventually cancelled
having never started. That failure has happened twice here: the required Store
lane spent a day at 45 attempts, 15 started and 27 cancelled while queued with
waits reaching 234 minutes, because exactly one runner carried `ci-store`; and
`store-shards.yml` pinned a shard to `ci-store-shard-1`, a label the fleet has
never had, which would have parked every pull request the moment the
accelerated path became required.

`make operations-check` now parses every `runs-on` in every file under
`.github/workflows/` — new files included, automatically — and fails when a
self-hosted label set no rostered runner can satisfy appears. So **adding a
label to a workflow means adding it to a runner first**: change the ansible
inventory, apply it, then add the runner to the roster in the change that
teaches a workflow to ask for it.

The roster also carries two invariants the same check enforces. `ci-store` and
`ci-topology` each select a three-voter hiqlite test, so at most one runner per
physical host may carry either, and neither may sit on a production plurx
voter. Two heavy replicated-cluster runs on one loaded host is the quorum flake
that produced the artwork-fence failures.

Today that means three `ci-store` slots — `gha-nuc1-general-01` on `nuc1`,
`gha-rogg16-general-02` on `rogg16` and `gha-nuc2-android-01` on `nuc2` — and
one `ci-topology` slot on `gha-rogg16-general-01`.

Because one host may hold only one slot, the Store shard count is *bounded by*
the number of slots rather than chosen: a surplus shard has no runner of its
own to take and serialises behind a busy one. `make operations-check` enforces
that bound in both directions, which is worth one worked example.

#### A runner is not a slot until the lane passes on it

`gha-nuc2-android-01` was made the third slot on 2026-09-02 and immediately
failed `replicated Store contracts (legacy)` three runs in a row — 2840, 2842
and 2845 — about twelve seconds into each job, before compiling anything:

```text
warning: failed to write cache, path: /home/runner/.cargo/registry/index/
index.crates.io-1949cf8c6b5b557f/.cache/as/yn/async-trait,
error: Permission denied (os error 13)
Permission denied (os error 13)
##[error]Process completed with exit code 2.
```

`gha-nuc1-general-01` (run 2838) and `gha-rogg16-general-02` (run 2837) ran the
same lane green in the same hour, so it was the host and not the lane. The
label was revoked, and the shard count had to drop with it — the roster edit
alone, with the workflow still asking for three shards, failed preflight with
`2 not greater than or equal to 3: the Store shard count exceeds the number of
ci-store slots`. That is the bound working: a fleet that loses a slot cannot
leave an unschedulable shard behind.

The cause was on the guest: `/home/runner/.cargo` was owned `root:root` and
`/home/runner/.cargo/registry` did not exist, because the runner role seeds
only the `bin` subdirectory and leaves the root-owned parent alone. Both are now
`runner:runner`, matching the actions-runner service user (uid 1000, gid 1001).

The label came back only after the lane was proved **on that exact runner**:
run 2840 attempt 3 was re-run while both other slots were busy so it had to
land there, and it passed in 27.9 minutes — in family with
`gha-nuc1-general-01` (25.7–29.1 min) and `gha-rogg16-general-02` (21.8–22.7
min). An ownership fix, a successful write probe and a convincing explanation
are not together evidence that a 27-minute three-voter hiqlite test will run on
a machine. One green run of that test is. **Add a `ci-store` label, then prove
it, then raise `SHARD_COUNT` — in that order.**

### Execution mode — fail-safe rollout without renaming the gate

`CI_EXECUTION_MODE` controls how eligible heavyweight lanes execute. It accepts
`legacy`, `shadow`, or `accelerated`; an unset or unknown value resolves to
`legacy`. In legacy mode the complete Store command is required and shards are
not scheduled. In shadow mode the complete command remains required while the
shard graph records comparison evidence through a separate reusable-workflow
call that no required job depends on; an offline shadow runner therefore cannot
delay the Store verdict or promotion gate. In accelerated mode the
verified shard union becomes required and the legacy job is skipped. The
stable `replicated Store contracts` verdict translates those internal results,
so branch rules and the promotion gate never depend on rollout-only job names.

```bash
scripts/ci-execution-mode status
scripts/ci-execution-mode legacy
scripts/ci-execution-mode shadow
scripts/ci-execution-mode accelerated
```

Changing the mode affects only new runs. The Store lane fans out to one shard
per `ci-store` slot — three today — and every shard selects the same label set
the legacy lane and the weekly backstop already use, so the sharded path
schedules on verified non-voter hosts rather than waiting on a label nobody has
assigned. Two is the floor `validation/store_shard.py` enforces, so the count
tracks the fleet up and down without ever collapsing into a single unsharded
job wearing the sharded lane's name.

Three shards on three slots trades throughput for latency, and it is worth
being clear which. One pull request's Store lane should finish in a fraction of
its present 22–29 minutes, but it occupies the whole `ci-store` pool while it
does; with several pull requests in flight the shards interleave as slots free
and aggregate throughput is roughly unchanged. The sharded lane has never run,
so the per-shard figure is unmeasured — and each shard builds its own test
binary, a fixed cost sharding does not divide. Rehearse with
`workflow_dispatch` and read the real numbers before flipping the variable. The weekly `replicated Store backstop` workflow is
unsharded in every mode and remains the assignment-independent safety net.

Prove the sharded path before flipping the variable, not after. `replicated
Store shards` accepts `workflow_dispatch` with an `execution-mode` input that
defaults to `shadow`, so a manual run exercises the same jobs on the same fleet
while `continue-on-error` keeps the result advisory:

```bash
# Forgejo → plurx → Actions → replicated Store shards → Run workflow
# Set execution-mode to shadow.
```

The dispatch entry point is only visible once the workflow is on the default
branch. A rehearsal that schedules, shards, and produces an aggregate receipt
is what earns `scripts/ci-execution-mode accelerated`; it is not a substitute
for the shadow campaign, only the cheapest way to find out whether the graph
can run at all.

Eligible packaging changes require both rows of the `package_smoke` matrix.
The arm64 row selects the self-hosted Linux/ARM64 `ci-arm64` VM pool. It proves
both the kernel and Docker engine are native `aarch64`, then performs the same exact-candidate binary
export, runtime-only image build, identity checks, health probe, and stop/start
smoke as amd64 without QEMU. Its manifest and digests are retained as required
qualification evidence, and `Main promotion gate` depends directly on the
complete matrix.

At least one `ci-arm64` VM must therefore remain online for self-hosted
qualification. The ARM package row shares the `plurx-apple-silicon-heavy`
concurrency group with Apple tests so laptop capacity is not oversubscribed;
`queue: max` preserves every pending required job instead of replacing an
older pending candidate.

Persistent lab runners verify their pinned ffmpeg, Playwright, XcodeGen, KVM,
and cross-compiler dependencies but do not mutate themselves; Ansible remains
the source of truth for installed toolchains.

### Which ffmpeg the profiles assume

Every CI profile runs **ffmpeg 6**, from a pinned Ubuntu 24.04 environment.
`.github/actions/ffmpeg` verifies the Ansible-provisioned build on a persistent
lab runner. It prints the build
that actually resolved into the job log and step summary, and fails the job
when the major is not the one that lane named. So the selected environment and
the expected major move together in a reviewable diff, and neither can move on
its own.

This is a deliberate choice rather than an inherited default, because the two do
not agree. **ffmpeg 8 declares `-readrate_initial_burst` and then ignores it**,
which costs the copy path its startup burst — the burst-then-hold behaviour
[PLAYBACK.md](PLAYBACK.md) promises — with no warning, because the capability
probe sees the option advertised. That is [#380](https://github.com/pjunod/plurx/issues/380),
and the plurxd side of it is #386. Before this pin, no CI job had ever run
ffmpeg 8; the gate was green by accident of whichever image `ubuntu-latest`
resolved to that week, and a promotion past 24.04 would have turned `main` red
in one silent step with no diff to blame.

So "green in CI" and "green on a worker" currently mean different things, and
this is the difference: a worker host on Ubuntu 26.04 runs ffmpeg 8.0.1, where
`make validate` fails on the pacing assumptions in
`crates/plurxd/src/transcode.rs` until #386 lands. The nightly `ffmpeg8-pacing`
job is where that gap is watched — it runs the same capability contract as the
`playback-recovery` point against a real ffmpeg 8, pinned by the `ubuntu:26.04`
container tag. It is nightly rather than required on purpose: making the gate a
matrix over both majors before #386 exists would leave a required check red by
design. `tests/operations/test_contracts.py` enforces the whole arrangement —
no job may install ffmpeg outside the action, name a major without pinning the
image or container that supplies it, or drop either major's coverage.

### Base syncs — convention keeps the qualified tree current

The private repository's current account plan does not expose branch
protection or rulesets, so GitHub cannot require an up-to-date head. Before
final qualification, merge `main` **into** the shared effort branch. Never
rebase that branch: rewriting every integrated task commit creates avoidable
review and recovery work.

`Main promotion gate` tests GitHub's merge tree at the base current when the
run starts and records that exact tree. If `main` moves before merge, the
operator merges it into the effort and qualifies again. This is presently the
"pretty please" rule: Paul controls every merge and does not merge a pending,
red, or stale candidate. If strict branch protection becomes available, the
same aggregate becomes the one required status check without changing the
workflow contract.

## Load-sensitive cluster checks — a timeout is not a verdict

One check drives real replicated infrastructure rather than a library, so the
host it runs on is part of the experiment:

| Check | Point | What the host can change |
|---|---|---|
| `cluster-auth` (`make cluster-check`) | `cluster.auth` · `persistence.upgrades` | Three voters run as separate processes and every call carries a three-second per-operation deadline (`STORE_TIMEOUT` in `crates/plurx-core/src/store/hiqlite.rs`). Under a full `make validate` those voters compete with every other check for the same cores, and that deadline is reachable by scheduling pressure alone |

**What a timeout there means.** `Database("replicated store operation timed out")`
is the host reporting that it could not finish an operation in three seconds.
It is not durable-state evidence in either direction: nothing was proved and
nothing was found broken. The production deadline stays at three seconds
because it is a server safety bound, so the suite absorbs load by re-attempting
a deadlined step from a reset target instead of by relaxing it.

**Why this check reaches that deadline before the others do.** The import
contract and the production bound push against each other by design. The
byte-budget transaction builder (#282) sizes every transaction as close to the
WAL payload capacity as it can, because a transaction that stays comfortably
small would not prove the bound it exists to prove. Maximising the payload also
maximises how long one replicated operation takes, so this check spends most of
its time on operations deliberately sized to sit near the three-second ceiling.
That is not only a test-harness concern: a real library import on a busy server
runs the same builder against the same fixed bound, so an operator seeing this
timeout in production is seeing the same interaction, not a different bug.

**How to tell it from a real regression.** The failures look different at the
client, and the check now says which of these three it saw:

- A **replicated deadline** names itself, states that the bound was neither
  proved nor violated, and points back at this section. Rerun `make
  cluster-check` alone on an idle machine; it takes about ten seconds.
- A **durable-state or size violation** carries the contract's own verdict.
  An oversized Raft transaction, for example, is refused by `hiqlite-wal` in
  the leader (`` `data` length must not exceed `wal_size` ``) and reaches the
  client as `ClientWriteError: panicked` — a byte comparison that reports
  identically on an idle and a saturated host.
- A deadline whose voter then **fails a consistent readiness read** is treated
  as the violation, not as load: a busy voter still answers that probe, while
  a leader killed by an oversized transaction does not.

Never re-diagnose a red `cluster-auth` from elapsed time. Read which of those
three the failure text claims, and reproduce it in isolation before treating it
as a durable-state regression.

### A live process without quorum is not ready to serve mutable media

`cargo run --locked -p plurx-cluster-check -- serving-partition` starts three
real voters plus a distinct serving process. Raw TCP cut-points isolate only
the serving process's remote Hiqlite client; the controller retains direct
access to every voter. The retained contract requires:

- with every cut-point enabled, leadership moves to another voter, the prior
  one-second authority lease ages out, and media admission recovers through a
  different member of the original configured proxy pool;
- closing an already-authenticated watermark stream through the first proxy
  advances that DB stream's pool cursor and obtains a later quorum watermark
  while the first proxy remains unavailable;
- `/healthz` remains 200 while `/readyz` changes to 503 from the expired
  production quorum watermark;
- mutable HLS capability traffic changes to a topology-free 503 with
  `Retry-After: 1`;
- the serving process reaps its live media child without another Store call;
- the intact voter majority commits a write and every voter process observes
  it from its own local replica during the serving partition; and
- restoring the cut-points returns readiness and capability traffic to 200;
  a newly admitted media child is then fenced by a second cut, proving proxy
  reconnect cannot escape the same boundary.

The proof compiles the daemon's `serving_fence.rs` directly. A harness-only
boolean would show that the test can notice its own partition, not that the
production readiness and teardown authority does.

The distinct process has no local Raft replica. Its monitor therefore renews
only the bounded quorum authority proof: it neither polls a remote management
metrics endpoint nor fabricates an applied index or apply-lag value. This mode
may gate serving readiness but is never eligible for bounded replica reads.

### A busy port is not an un-migrated store

A voter's raft and API ports are chosen by binding port zero, reading the
port, and releasing the listener. Nothing holds them: the process that binds
them for real is a child started afterwards, and hiqlite binds its own sockets
from an address string, so there is no listener to hand over. Under
gate-parallel load another process can claim one of those ports in between,
and that is an environment fact rather than durable-state evidence.

It used not to read as one. hiqlite serves both listeners from detached
`tokio::spawn` tasks that `.unwrap()` the serve future
(`hiqlite-0.14.0/src/start.rs:148` and `:231`), so a losing bind panicked a
background task and nothing else: `start_node` had already returned `Ok`,
`wait_until_healthy_db` probes only the *local* database, and the voter
announced readiness with a dead listener. The collision then surfaced as
whatever the crippled voter failed at next:

| Port lost | What the gate used to print |
|---|---|
| raft | `no such table: cluster_meta` — a linearizable read reaching a state machine that never applied the schema batch, which is exactly what a genuinely un-migrated store looks like |
| API | `replicated store operation timed out`, then `auth store has not been opened` |

Both of those are contract verdicts, and the first is a serious one. Neither
was true. A voter now proves both of its listeners accept before it announces
readiness, and a bind failure is reported as `port collision: …` and stops the
voter, so a busy port cannot go on to be reported as durable-state damage.
Cluster starts reallocate their ports up to five times on that classification
and on nothing else — see `is_port_collision` and `with_port_retry` in
`crates/plurx-cluster-check/src/lib.rs`.

**How to tell them apart.** Read the verdict, never the elapsed time. A `port
collision:` failure names the loopback address that collided and means nothing
was proved; rerun the check alone. Anything else is the contract's own answer
and is reproducible on an idle host. Note that the API-port case above shares
its `replicated store operation timed out` text with an ordinary replicated
deadline — the difference is that a collision now says so first, so a timeout
arriving on its own is still the deadline it has always been.

### PortReservation — the listener is held until the voter starts

The residual race between `allocate_nodes` (which binds, reads the port, and
releases the listener) and the child process binding the same port from its
address string cannot be eliminated on a general OS: the child is a separate
process, and there is no mechanism to hand a listening socket across
`Command::spawn`. The race is made as short as possible in two ways:

1. **`PortReservation`** (`crates/plurx-cluster-check/src/lib.rs`, added for
   issue #381). `allocate_nodes` now returns a `PortReservation` whose
   listeners are held alive until the caller consumes it. The site that starts
   the child process — `ClusterProcesses::start` — holds the reservation
   through the `NodeSpec` construction and drops it immediately before
   spawning, so the window between "port released" and "child binds" is the
   narrowest possible sequence of `drop` + `Command::spawn`.
2. **`start_cluster_with_port_retry`** wraps every start in `with_port_retry`,
   which retries the entire allocation up to five times on `is_port_collision`
   and on nothing else. A collision on a transiently occupied port is
   self-healing; a collision on a permanently held port fails with a message
   naming the collision, never a durable-state verdict.

The two mechanisms are complementary: `PortReservation` closes the window as
far as the OS allows, and the retry loop answers any collision that still
manages to slip through before the child binds.

### Previous instances of the same defect shape

This is the fourth issue whose root cause is a check that cannot tell its
environment from the contract it asserts:

- #315: benchmark wall clock reported as a performance regression.
- #368: replicated deadline reported as a WAL-size violation.
- #374: data-directory lock reported as a second daemon.
- #381: port collision reported as an un-migrated store (this issue).

## The UI golden — a saved answer key, not a magic test

“Golden” is testing jargon for a reviewed, known-good output saved in the
repository. The test makes the same output from the current application and
compares the two:

```text
known-good UI structure ──▶ tests/ui-structure.golden
current UI structure    ──▶ target/ui-baseline/structure.txt
                                      │
                    same ── pass ◀────┴────▶ different ── fail + readable diff
```

In plurx, the committed golden is **not a screenshot**. For every supported
layout, route, and viewport, it records portable structural facts:

- which DOM shapes and accessibility states exist, and how many;
- the keyboard tab order;
- the API method and path calls made while rendering; and
- the registered layouts and the surfaces on which they are allowed.

The same browser pass also fails on deterministic accessibility defects:
duplicate IDs, broken ARIA references, unnamed interactive controls, and
images with no `alt` contract. Those rules are direct assertions, not saved in
the golden; the golden preserves the larger structure and keyboard/request
behavior after those assertions pass.

It deliberately excludes pixels, timestamps, local paths, and arbitrary page
text. Pixel rendering changes with the browser, fonts, and machine, so
screenshots and their hashes stay under `target/ui-baseline/` for optional
same-machine before/after comparison. They are never the committed golden.

The golden is needed because the web app is generated in the browser from one
large embedded HTML/JavaScript file. Rust can compile and pass its tests while
a class, `aria-disabled`, tab stop, route request, or entire JavaScript-rendered
screen has changed. The saved answer key gives those browser-visible contracts
something automatic to compare against. It is not required by the validation
framework in general; it is evidence for the `web.experience` functionality
point specifically.

There are two legitimate outcomes when `make ui-check` reports drift:

| What happened | What to do |
|---|---|
| The UI change was accidental | Fix the UI and rerun `make ui-check` |
| The UI change was intentional | Run `make ui-golden`, inspect the golden diff, then commit it with the UI change |

Never regenerate the golden merely to make a failure disappear. Rewriting the
expected answer without reviewing the diff turns the check into an automatic
approval of whatever the application did.

The repository contains the activated `tests/ui-structure.golden`: 54 captures
across every registered layout, route, and desktop/mobile viewport. A check
rebuilds `plurxd` first so it cannot accidentally serve an old embedded web
app, then runs a real browser sweep. It needs Python Playwright with Chromium,
`ffmpeg`, and a buildable `plurxd`; expect it to take minutes rather than
seconds. The generated player media includes a synthetic audio track so the
browser exercises a real media timeline, but validation browsers always launch
with host output muted and operating-system media-key integration disabled.
Audio is still decoded and selectable; the gate neither sounds the workstation
speakers nor claims macOS Now Playing.

```bash
make ui-golden          # capture the current reviewed UI as the answer key
git diff -- tests/ui-structure.golden
make ui-check           # prove a fresh capture matches it
```

`make ui-golden` is only for an intentional reviewed UI change. Ordinary local
and CI runs use `make ui-check`; they never rewrite their own expected answer.

## Run it — plan first when the impact is surprising

```bash
scripts/validate list                                      # inventory every point
scripts/validate lint                                      # schema + path coverage
scripts/validate plan --profile commit --staged            # explain a staged change
scripts/validate run --profile commit --staged             # what the hook runs
scripts/validate run --profile full --point playback.pipeline
make validate-help                                         # Makefile-oriented guide
make validate-plan                                         # staged plan, no checks
make validate-staged                                       # staged commit profile
make validate                                              # commit profile, every point
make validate-full                                         # every runnable deep check
make validate-nightly                                      # exhaustive scheduled tier
make history-check                                         # every past fix has evidence
make operations-check                                      # deploy/CI/ship contracts
```

**How to read the plan:** `because path:...` is a direct match;
`because consumer:...` was pulled downstream from a changed provider. “No check in
this profile” means the contract is known but its evidence belongs to a deeper
or platform-specific profile. It does not mean the point passed.

**How to read the result:** `passed` means the command returned zero · `failed`
means the command or a required prerequisite failed · `skipped` names an
unavailable tool, file, platform, or explicit skip variable. Point status in
`report.json` is `passed` · `failed` · `partial` · `not-covered` · `not-run`.
Only executed evidence earns `passed`.

Install the pre-commit hook once:

```bash
make hooks        # copies scripts/pre-commit into .git/hooks/pre-commit
```

The hook uses the staged diff, not unstaged experiments beside it. It still
runs the mandatory baseline on every commit, then adds any point-specific
commit checks.

## Behavior fixes — prove the test distinguishes the correction

A corrective pull request should leave behind at least one test that passes on
the pull request and fails when only the production correction is restored from
the base tree. `scripts/prove-fix` performs both runs inside a disposable local
clone; it never rewrites the checkout in which it was invoked.

```bash
# Rust: filter one retained test and restore one or more production files.
scripts/prove-fix origin/main repaired_case crates/plurxd/src/example.rs

# Swift: use the same protocol on macOS with the shared simulator suite.
scripts/prove-fix --command 'make apple-test' origin/main - \
  clients/apple/Sources/PlayerView.swift
```

Both proof runs compile into an isolated cargo target directory, never the one
an ordinary `cargo` or `make validate` invocation uses. This is mandatory, not a
convenience: the second run compiles reverted source, and cargo's freshness
check is mtime-based, so a shared target directory leaves the pre-fix artifact
newer than the corrected checkout and the next gate silently reuses it. The
isolated directory is a sibling of the inherited `CARGO_TARGET_DIR`
(`<dir>-prove-fix`), so proofs still reuse each other's dependency builds and
pay one cold workspace build rather than one per invocation; with no inherited
value it is a directory inside the disposable clone, matching what cargo would
have done anyway. `tests/validation/test_prove_fix.py` asserts the isolation so
a refactor cannot quietly reintroduce the sharing.

The first run must be green. The second run must be red after the named
production paths are taken from the base revision. A test that stays green is
evidence that the patch touched a test, not that the test protects the fix.
Pull requests may opt into the first-month report-only CI job with the
`fixes-behavior` label. It applies the protocol to changed Rust production
files and writes the result to the job summary; Swift uses the documented
manual command because Linux cannot run XCTest.

Corrective client commits also add or update a row in
[`tests/client-fixes.toml`](../tests/client-fixes.toml). Each row binds the
commit to a current production symbol and a current test symbol. Removing or
renaming either anchor fails the validation inventory. This is deliberately
stronger than accepting any test edit in the same commit: an unrelated test
cannot satisfy the post-review history policy.

The scheduled workflow adds a second, exploratory layer. `cargo-mutants` is
limited to Rust files changed in the previous seven days, gives each mutation
300 seconds, and has a 60-minute job cap. Its report is uploaded from
`target/mutants`; surviving mutants are summarized but do not fail the nightly
workflow during the first signal-gathering month.

## Historical fixes — every scar names the check that guards it

Path ownership answers which promise a change can affect; it does not prove
that the checks reproduce bugs already encountered. `make history-check`
closes that gap by walking every non-merge corrective commit reachable from
`HEAD`—including conventional `fix` commits, plain-English correction verbs,
compatibility corrections, and `perf` fixes—and requiring one of two forms of
evidence:

- for older commits, the fixing commit added or changed a surviving test or assertion;
- for client corrections after the review baseline, a client-fix anchor names
  the production-to-test relationship;
- for other runtime corrections after that baseline, an explicit regression
  mapping or anchor names the current evidence; or
- a fragment in [`validation/regressions.d/`](../validation/regressions.d/)
  explicitly maps the commit to a current functionality point and runnable
  check.

The explicit mapping is for checks whose evidence lives outside the fixing
patch: both Apple platforms compiling, a real container completing its
non-root lifecycle, the browser playback matrix, or the UI structural sweep.
It is not an exemption. Unknown commits, checks, and points fail; duplicate
claims fail; and a new corrective commit with neither a test nor a mapping
fails the baseline.

### One mapping, one file

The mapping ledger is a directory, not a file. Each entry lives in its own
`validation/regressions.d/<first-commit-prefix>-<slug>.toml`, holds exactly one
`[[coverage]]` table, and repeats `version = 1`. The audit loads every `*.toml`
there in file-name order and treats them as one ledger; entry order carries no
meaning.

```toml
# validation/regressions.d/a1b2c3d4-playback-pipeline.toml
version = 1

[[coverage]]
commits = ["a1b2c3d4"]
points = ["playback.pipeline"]
checks = ["rust-gate"]
reason = "One sentence naming the current check that exercises the defect."
```

The file name is not cosmetic. The audit rejects a fragment whose name does not
begin with its own first mapped commit, so two corrective changes can never
choose the same path. That is the whole point of the directory: a single
append-only `validation/regressions.toml` put every new entry at the same
offset, so each merge to `main` conflicted every other open pull request
carrying a mapping — and clearing that conflict with a rebase rewrote the SHAs a
reviewer had pinned an approval to, spending a review cycle on nothing. Adding a
file collides with nothing. The loader refuses to run while a shared
`validation/regressions.toml` exists, so the hotspot cannot come back.

[`validation/regressions.d/README.md`](../validation/regressions.d/README.md)
carries the field-by-field format next to the entries themselves.

```bash
make history-check
# history ok: 288 corrective commits · 213 direct test changes ·
#             75 explicit current-check mappings · 4 client-fix anchors ·
#             9 non-runtime corrections
```

The exact counts grow with the repository. Read the result as a coverage audit:
every corrective commit must have direct test evidence, an explicit executable
check mapping, a client-fix anchor, or a reason proving that it changed only
documentation, comments, or ignored generated state. The evidence counts cover the number
audited. The machine-readable inventory is written to
`target/validation/history.json`, including the subject, functionality points,
and coverage route for every commit.

Operations get a second, focused layer because several real failures were
valid shell/YAML that pointed at the wrong target. `make operations-check`
pins the Compose project and state mount, configurable HTTP/discovery ports,
discovery networking, build stamp, real Apple/Android ship targets, concrete
CI simulators, container port/state/cleanup behavior, and copy-video reporting.

The same operations gate extracts current mobile build claims from the Apple
and Android READMEs, the Apple parity document, and the status page. Those
claims must match `CURRENT_PROJECT_VERSION` and `versionCode`; advancing a
client build without its user-facing status documents is red before a PR opens.

The READMEs and the parity document each carry exactly one anchored `> Status:`
line, so the gate reads a single declared current claim from them. `STATUS.html`
has no such anchor — it is prose, and every Apple build number in it is treated
as a current claim. That page also has to narrate history, and a sentence like
"build 52 corrected the reported iPad mini failure" stays true after the next
bump while a regex cannot see its tense. Mark such a mention explicitly:

```html
<span data-build-history>Apple build 52</span> corrects the reported iPad mini failure
```

Marking is the exception, never the default. An unmarked mention is still swept,
so a status page that falls behind a build bump is red; the only way to exempt
one is to write the marker around it, which is a reviewable edit. Use it only
for mentions that are genuinely about a past build — rewording a true sentence
to dodge the sweep degrades the page for its readers.

The exemption is granted only by the marker itself: a bare `data-build-history`
attribute — or one with an empty value, which is how a formatter serializes a
boolean attribute — on a real `<span>` element that closes around its own
sentence. The gate parses the page rather than pattern-matching its text, so the
marker is recognized only at attribute-name position on a tag the HTML tokenizer
actually produces. A name match alone also fires on markup that is not the
marker, and these all read as ordinary mentions and stay swept:

```html
<span title="data-build-history">Apple build 52</span>   <!-- a value, not the marker -->
<span data-build-history-note>Apple build 52</span>      <!-- a different attribute -->
<span data-build-history="false">Apple build 52</span>   <!-- the marker takes no value -->
<span data-build-history/>Apple build 52</span>          <!-- malformed opening tag -->
```

Marker-shaped characters that are not an element grant nothing either. A comment,
another element's quoted attribute value, and `<script>` or `<style>` text can
all spell an opening or closing tag without being one; the build number between
them is still on the rendered page, so it is still a claim:

```html
<!-- <span data-build-history> --><p>Apple build 52</p><!-- </span> -->
<div title="<span data-build-history>">Apple build 52</div>
```

Everything else fails closed the same way and returns the mention to the sweep:
an unclosed marker span, the marker on any element other than `<span>`, and a
marker that drifts across a nested `<span>` and so no longer wraps its own
sentence. In the other direction, HTML's own insignificant variation is honored,
so a formatter cannot turn a correctly marked sentence red: attribute order,
attribute-name case, surrounding whitespace and line breaks, and a quoted
attribute value containing `>` all still match. `tests/operations/test_mobile_build_claims.py`
pins both lists.

Android needs no equivalent. Its build claim is read only from the anchored
`> Status:` line in `clients/android/README.md`, and no whole-file sweep runs
against Android numbers anywhere, so a historical Android mention cannot break
the gate. `STATUS.html`'s Android build numbers are consequently unvalidated —
the opposite gap, and out of scope here; adding that coverage would mean
adopting the same marker for the historical Android mentions the page already
carries.

### Who owns the Apple build number

The number is claimed once, in `clients/apple/project.yml`. Every other copy is
**generated** from it by `make apple-build-bump`
(`validation/apple_build.py`), and `tests/operations/test_apple_build_claims.py`
re-renders the tree to prove no copy was hand-edited. The generated copies are
exactly the ones the sweep above already reads:

| Surface | Occurrence |
| --- | --- |
| `clients/apple/project.yml` | `CURRENT_PROJECT_VERSION` — the only load-bearing claim |
| `clients/apple/README.md` | the anchored `> Status:` line |
| `docs/APPLE-CLIENT-PARITY.md` | the anchored `> Status (date):` line |
| `docs/STATUS.html` | the viewers tile and the `👤 Paul` TestFlight upload item |

Per-build narrative is **not** one of them. It lives in `docs/apple-builds/`,
one file per change, named after the issue rather than the build. Before issue
#509 that narrative sat at a shared insertion point in the two `> Status:`
blockquotes and in a growing parenthetical on the `👤 Paul` item, so any two
concurrent Apple branches conflicted on all three and re-conflicted every time
`main` moved — while the number itself merged clean, because both branches
wrote the same next value, into a tree where that value was already taken.
Builds through 78 are archived verbatim in
`docs/apple-builds/history-through-build-78.md`.

The relocation is enforced, not merely documented: neither anchored `> Status`
blockquote may contain a `Build <n> …` sentence. The ban stops at the
blockquote. Body prose may still narrate a past build — `APPLE-CLIENT-PARITY.md`
explains under its own headings how a behaviour came to be — because those
sentences never accumulate at a shared offset, and banning them would push
authors to reword true prose, which is the same failure `data-build-history`
exists to prevent on `STATUS.html`.

Nothing above is relaxed. `validation/mobile_versions.py` still requires the
counter to increase past the **merge target** whenever Apple release inputs
change, and the claim still has to agree across all four documents. What changed
is that re-claiming after the base moves is `git merge origin/main` followed by
`make apple-build-bump` — a sync Git resolves by itself plus one mechanical
commit — instead of three prose merges and six hand-edited mentions that had
already merged clean and wrong. The order matters: claiming before the sync
leaves the branch two above the base while `main` holds one above it, on the
same line.

The four `👤 device` acceptance items in `STATUS.html` deliberately no longer
name a build. They are forward-looking instructions rather than evidence, so
"on the current Apple build" is both truer and one fewer copy to drift.

## Add a functionality point — define the behavior before its command

Start with the promise and the code capable of violating it. Then attach the
smallest existing check that proves the promise. Add a new command only when
the current test layers cannot observe the behavior.

```toml
[[points]]
id = "search.results"
title = "Cross-library search"
contract = "A viewer sees authorized movies, series, and episodes ranked by a stable query."
paths = [
  "crates/plurxd/src/http/search.rs",
  "crates/plurx-core/src/store/sqlite/media.rs",
  "clients/**/Search*",
]
checks = ["rust-gate", "search-contract"]
depends_on = ["identity.access", "library.catalog", "server.api"]
```

Checks are declared once and reused:

```toml
[[checks]]
id = "search-contract"
title = "Search API contract cases"
command = "cargo test -p plurxd http::search::tests -- --nocapture"
profiles = ["commit", "ci", "full", "nightly"]
requires = ["cargo"]
missing = "fail"
timeout_seconds = 120
```

The supported path syntax is repository-relative `*` · `**` · `?` · brace
choices such as `{watch,trakt}.rs`. Keep globs narrow enough that the plan
explains a real cascade, but broad enough that a newly added file cannot evade
the contract.

Run these acceptance checks after editing the catalog:

```bash
scripts/validate lint                                      # no orphan source files
python3 -m unittest discover -s tests/validation -p 'test_*.py'
scripts/validate plan --profile ci --paths path/you/changed.rs
```

`lint` audits every governed tracked or untracked source file. Its coverage
target is exact: 100% of `settings.audit_paths` must match at least one point,
and 100% of points must name at least one valid check. This is catalog
coverage, not a claim that every behavior is exhaustively tested.

## Current inventory — what is actually validated

| Functionality point | Commit / CI evidence | Full-profile addition |
|---|---|---|
| `core.media` | Workspace Rust tests, formatting, and clippy | Scheduled resource-bound checks reach downstream consumers |
| `persistence.upgrades` | Focused historical migrations, reopen persistence, and refusal of future schemas | Same focused contract |
| `server.api` · `api.contract` | Seeded read/write journeys plus one canonical JSON fixture checked against live Rust responses and decoded by Swift and Kotlin | Native simulator suites exercise their decoders too |
| `library.catalog` · `metadata.enrichment` | Scan, browse, matching, retry, and user-owned metadata tests | Downstream browser/client surfaces run when a provider changes |
| `identity.access` | Focused anonymous/viewer/admin/scoped-key boundaries | Same contract across native client suites |
| `playback.pipeline` | Rust decision, delivery, stream, HLS, and session tests | Risk-weighted browser matrix; nightly exhaustive quality/restart and interruption recovery |
| `watch-state.sync` | Seeded progress/watched/settings journey plus store and Trakt tests | Same journey through deeper consumers |
| `offline.viewing` | Store/API contracts cover ownership, quotas, stable hashed leases, expiry, package media, and recovery; Swift and Kotlin suites cover client contracts | Native simulator/device transfer restoration, cache-only playback, and disconnected physical-device evidence |
| `plex.compatibility` | Plex discovery, metadata, media, playback, and timeline tests | Browser playback when shared delivery changes |
| `web.experience` | JavaScript syntax, theme contrast, 36-capture structural golden, keyboard/request invariants, and accessibility smoke | Risk-weighted shipped-player matrix; nightly exhaustive quality and restart cases |
| `apple.client` | Dedicated iOS and tvOS simulator CI, sharing one XCTest source | Local/full simulator run on macOS |
| `android.client` | JVM models/policies and lint plus dedicated instrumented Compose/TV-focus CI | Explicit disposable-device run for `full` |
| `operations.packaging` | Compose, CI, ship-target, port, state, build-stamp, and report contracts plus release builds | Image must start non-root, become healthy/ready, expose metrics, restart, keep its instance identity, and clean up |
| `validation.framework` | Catalog schema, historical-fix coverage, cycles, duplicate IDs, glob/audit coverage, staged renames, cascade selection, timeout/fail-fast, and report tests | Same contract |

The cross-client fixture is `tests/contracts/native-api.json`. It is not a
second server implementation: Rust compares representative keys and JSON types
with live handler responses, while Swift and Kotlin decode the same bytes with
their production models. Adding a field remains compatible; renaming a field
or changing its type fails at the consumer boundary where the drift matters.

Line coverage remains a diagnostic badge, not the acceptance target. A high
line percentage can execute code without asserting its behavior; the catalog
instead makes each important promise name the evidence meant to protect it.

## Evidence — enough detail to reproduce the failing layer

Every run writes under `target/validation/`:

| Artifact | What it answers |
|---|---|
| `report.json` | Which points were selected, why, their profile coverage, and each check result |
| `junit.xml` | CI-ingestible pass, failure, and skip cases |
| `logs/<check>.log` | Complete combined output for the check command |
| `history.json` | Every corrective commit and whether direct tests or an explicit current check cover it |

CI uploads that directory even when a check fails. Read `report.json` first to
find the affected promise, then the named log to diagnose the failing layer.

## Non-goals — what the framework does not pretend to prove

- **It does not infer behavior from code.** Humans define the contract and its
  path boundary; the audit only prevents governed files from having no owner.
- **It does not replace focused regression tests.** A point routes tests. The
  test itself should reproduce the bug and assert the intended result. The
  historical ledger is reserved for evidence that necessarily lives in a
  platform, browser, or runtime check outside the fixing patch.
- **It does not make hardware interchangeable.** Browser playback, simulators,
  and JVM tests cannot prove Dolby Vision on a physical Apple TV or TV remote
  focus on every Android device. Keep those device observations explicit.
- **It does not use line coverage as a release verdict.** Coverage locates dark
  code; functionality points say which promises matter when that code changes.
