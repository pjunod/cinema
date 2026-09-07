# CI execution acceleration — Fable review

**Status:** review complete · **Reviews:**
[CI_EXECUTION_ACCELERATION_PLAN.md](CI_EXECUTION_ACCELERATION_PLAN.md) ·
**Verified against:** `origin/main` @ `1a64fa8a` (2026-09-01) ·
**Written:** 2026-09-01

**Verdict: approve the architecture; revise before implementation.** The
four-part design — keep GitHub scheduling, runner-local caches with a
Forgejo registry fallback, a native ARM64 lane, split-then-shard the Store
suite — attacks the measured causes, and the invariants in §3 are the right
ones. Three blockers (§2) and two structural recommendations (§3) below;
answers to every §15 question in §4; ledger recommendations in §6.

---

## 1. What was verified — don't re-derive these

Checked by reading the pinned tree (`git show origin/main:<path>`), not the
worktree. The plan's repository claims are accurate where checkable:

| Claim | Result |
|---|---|
| §2.2 docker job: per-job builder, `type=gha,mode=min`, amd64 build+load+smoke, arm64 QEMU `cacheonly` with no runtime probe | Confirmed in `ci.yml` |
| §2.3 `store_contract.rs` is 21,070 lines; `HIQLITE_CASE` process-global mutex; `--test-threads=1` in `cluster-store-check` | Confirmed, all three |
| §5.3 Dockerfile mounts registry + two targets, no `git` mount, no `sharing=locked` | Confirmed — the proposed hardening is right |
| §7.2 `validation/release_dockerfile.py` + `publish-release.yml` prebuilt/digest path exist | Confirmed — but the path is broken on main, see B1 |
| §5.2/§11 variable convention | `vars.CI_RUNNER_MODE == 'github'` already gates every `runs-on`; the plan's conditionals match house style |
| §10 effort process | Conforms to AGENTS.md large-effort rules |

The §2.1 run timings were not re-verified (the GitHub API is unreachable
from this review environment); their shape is consistent with the verified
workflow structure, and Milestone 0 re-measures them anyway.

---

## 2. Blockers — fix before or inside the named milestone

### B1. The release packaging path Milestone 4 reuses is broken on main (execution-proven)

`validation.release_dockerfile.render()` **rejects the current
Dockerfile**. Since #560 (`b7521cb8`, 2026-08-25) the runtime stage copies
*two* binaries from the build stage — `plurxd` and `plurx-cluster-check`
(shipped as stopped-node operator tooling) — and `render()`'s
forbidden-token check fires on the second `COPY --from=build`. Ran it
against the pinned tree:

```text
ValueError: generated packaging Dockerfile retained build tokens: ['COPY --from=build']
```

Consequences:

- `publish-release.yml`'s package-and-smoke job fails at the render step
  for any tag whose tree includes #560 — **v0.3.0 (2026-08-31) is such a
  tree**. Whether that publish ran at all is a separate question (it runs
  on `ubuntu-latest`, and GitHub-hosted runners have been billing-blocked
  account-wide since 2026-08-30), but the path is broken either way. This
  is a live release-pipeline defect independent of this plan.
- The §7.2 artifact contract ships only `plurxd`. The runtime image needs
  `plurx-cluster-check` too, and `ci.yml`'s `build` job builds only
  `-p plurxd`. Prebuilt packaging as specified would produce an image
  missing a shipped binary — or fail, as above.
- The guard test (`test_generated_dockerfile_keeps_only_the_tagged_runtime`)
  renders a **synthetic fixture**, not the repo's Dockerfile, which is why
  a 7-day-old breakage was invisible. Same failure class as the
  tests-must-pin-the-call-site rule.

Required: teach `render()` the explicit two-binary contract (still
forbidding `FROM rust:` / `cargo build`); extend the per-arch release
artifact and `build-manifest.json` to both binaries with both digests; make
the operations test render the *actual* `Dockerfile`; and decide where
`publish-release` runs while hosted runners are blocked. Fixing the release
pipeline should probably land as its own small PR before this effort — it
is broken today, not hypothetically.

### B2. New per-run inter-job artifacts land on the account's scarcest resource

§7.1 has every container-selected run upload two release binaries as
GitHub artifacts to connect `release-<arch>` to `package-smoke-<arch>`.
This account is already "afoul of most of GitHub's quotas": artifact-quota
recalculation has blocked an effort qualification for 6–12 h before, and
artifact-upload deaths are the canonical infra-only failure. Today's
`build` job deliberately retains **no** large artifacts on ordinary PRs
(upload only on push/qualification, `continue-on-error` on push). The plan
would put the *fastest* lane behind the *flakiest* quota.

Fix: collapse build and packaging into **one job per architecture** —
compile, package via `release_dockerfile`, and smoke on the same runner, so
the binary never leaves the workspace. The `docker-smoke-gate` then needs
only manifests and digests (kilobytes). Keep the existing
push/qualification-only binary retention rule unchanged. If a cross-job
hand-off is ever genuinely needed, route it through the Forgejo registry,
not GitHub artifacts. This also removes two jobs from the graph.

### B3. The "no build labels on production voters" invariant is probably violated today — audit first

All four plurx nodes (nynuc, m6, nuc4, nuc3) are production voters, and the
runner inventory includes nynuc with its own standalone play. The heavy
jobs select on generic labels (`general`, `high-cpu`) across the 19-runner
fleet. If any voter currently carries those labels, the plan's invariant is
already broken, and honoring it *removes* capacity every later milestone
assumes. Milestone 0 must therefore produce a host ↔ runner ↔ label audit
artifact before any scheduling decision — it changes the answers to Q7 and
the shard count. Note also a tension to resolve deliberately, not by
accident: Paul already designated **nuc4 (a voter) as the fleet image build
node** in the Forgejo registry plan. Either the invariant carves out
deploy-time builds explicitly, or that choice gets revisited — the plan
should say which.

---

## 3. Structural recommendations

### R1. Decouple both wins from the laptops — most of the speed needs no new hardware

Sharding needs *distinct* hosts, not *ARM* hosts. nuc1 and nuc2 are runner
hosts and are not in the plurx voter roster; if the B3 audit confirms them
(or other fleet members) as non-production x86 builders, Phase A + two-shard
Phase B ship with zero new hardware and a homogeneous environment — the
32 m → ~14 m path does not wait on VM provisioning.

Likewise most of the Docker win is prebuilt packaging plus persistent
caches, not native ARM: amd64 drops to package+probe, and the arm64 QEMU
path loses its Cargo compile — the dominant cost — leaving runtime-stage
apt layers that the persistent builder caches. Native ARM then upgrades the
arm64 proof from "packaged and probed under QEMU" to "ran natively", which
is worth having, but it is the garnish, not the meal.

Suggested value order: M0 → M1/M2 (caches) → M5 (split) → M6 on x86
(shards) → M4 (prebuilt smoke, with B1/B2 fixed) → M3 (native ARM, shadow).

### R2. The M3 Max already has a required CI tenant — size and schedule around it

The `apple` job requires a self-hosted **macOS ARM64 xcode-26** runner. If
that runner is this same M3 Max, §6.2 double-books the machine: Xcode +
simulators, plus a 10–12 vCPU / 24–32 GB Linux VM, plus Docker, plus a
cluster shard, plus Paul actually using his laptop. The resource envelope
must subtract the existing tenant, and the scheduler must not co-schedule
the apple job with the VM's heavy jobs. Separately: cluster *timing* tests
on a thermally and load-variable host is how the last month of flake
archaeology started — keep replicated shards off laptops entirely.

---

## 4. Answers to the §15 questions

1. **ARM runtime.** Yes — a Linux ARM64 VM is the right contract: one
   environment compiles, packages, and runs the Linux artifact, which
   Docker Desktop packaging cannot claim. Subject to R2.
2. **Required or shadow.** Shadow-only until dedicated always-on ARM
   hardware exists. A required gate on a sleepable laptop violates the
   plan's own §3.2 non-goal; promote only after the availability sample.
3. **Prebuilt smoke coverage.** Preserved once B1 is fixed, with the
   focused source lane. Inputs that must force `docker-source-build`:
   `Dockerfile`, **`.dockerignore`** (it defines the build context — a bad
   ignore breaks only the in-Docker build), every `Cargo.toml`,
   `Cargo.lock`, the pinned toolchain input, `vendor/**` (vendored hiqlite
   compiles in the builder stage), any `build.rs`/build-helper input, and
   `validation/release_dockerfile.py` itself. Fail-open on unknown inputs,
   selector in `points.toml` — both already proposed, both right.
4. **Registry fallback in M2.** Yes — the Forgejo registry on nuc3 is
   already decided and its repo half merged (#739); read-mostly with
   protected per-arch writers is the right shape and the scopes are narrow
   enough. Implementation note: the `docker-container` builder does not
   inherit the daemon's `insecure-registries` — the builder needs its own
   buildkitd config (`[registry."192.168.4.7:3000"] http = true`) or every
   cache pull fails quietly to cold.
5. **Budgets.** The arithmetic doesn't close on the primary host: §5.5
   wants 100 + 60 + 20 GB of budgets *plus* a 100 GB-or-20 % floor
   (~280 GB), while §6.2 gives the M3 VM "at least 180 GB". Scale budgets
   per host — reserve the floor first, split the remainder roughly 50/30/20
   — or provision ~300 GB. Inventory real disk on every intended builder
   (NUC-class hosts included) before committing numbers.
6. **Hash vs manifest.** The receipt + exact-union validator is the
   strongest single mechanism in the plan — keep it verbatim. Hash
   assignment is fine to *start*, but it balances test count, not seconds:
   per-test cost here spans ~two orders of magnitude (three-voter cluster
   starts vs in-memory SQLite), so a 2-shard hash split will routinely land
   lopsided. Receipts already record durations; plan assignment-v2 as
   deterministic LPT over a committed duration table, validated by the same
   union check. Also, run **one cargo invocation per shard** passing all
   selected names (the libtest harness accepts multiple filters with
   `--exact`) instead of one invocation per test — same warm objects, ~100
   fewer cargo startups per shard.
7. **First shard hosts.** Prefer two non-production x86 builders if the B3
   audit leaves any — homogeneous with every historical baseline and no
   laptop availability question. Heterogeneous ARM shards only if x86
   capacity genuinely doesn't exist.
8. **Architecture-risk bound.** Yes, the scheduled unsharded x86 campaign
   is sufficient. Skip the monthly salt rotation: it churns the duration
   table and buys nothing the scheduled run doesn't already prove.
9. **Campaign lengths.** 20 runs only detects flakes at roughly ≥14 % with
   95 % confidence, and this suite's history is *rare* timing flakes.
   Accept 10 + 20 as the cutover bar, but add: any cluster-lane flake in
   the first 30 days after cutover returns that lane to shadow
   automatically, and the legacy lane stays runnable the full 30 days
   (§11 already keeps it).
10. **Fixture reuse.** Keep it a separate effort even if p95 misses —
    §8.5's own conditions are nearly a full effort, and it changes test
    semantics where everything else in this plan changes scheduling.
11. **Rollout variable.** Yes — three explicit values failing safe to
    `legacy` is the right boundary. Keep `CI_EXECUTION_MODE` distinct from
    the existing `CI_RUNNER_MODE` and document both in the same place;
    `pr_gate` stays the single required check name. 30 days of legacy
    retention.
12. **What's missing.** (a) the B3 host/label audit as a Milestone 0
    deliverable; (b) a disk inventory of intended builders (Q5); (c) an M0
    measurement of `Swatinem/rust-cache` save/restore cost per job, so the
    conditional in §5.2 is justified by a number; (d) a
    wrong-candidate-tree injection in §7.5 (it has digest and architecture
    mismatches — add a manifest whose `git_tree` names another commit);
    (e) a decision on where the shard duration table lives and how it
    updates.

---

## 5. Small corrections to the text

- **§5.2 snippet bug:** `CARGO_TARGET_DIR: >-` is a YAML *folded* scalar —
  the two lines join with a **space**, yielding
  `…/rust-1.97.1/ x86_64-unknown-linux-gnu/cluster-store`. Write it on one
  line. Also derive the `rust-1.97.1` path epoch from the pinned toolchain
  input rather than repeating the literal in two places.
- **§7.4 rename scope:** the smoke script's own messages say "restart"
  (`container did not become ready after restart`). Renaming to
  `container stop/start` therefore touches `scripts/container-smoke`
  text — a script change §7.4 otherwise forswears. Carve out the
  message-text exemption explicitly so the implementer doesn't stall on it.
- **§2.3/§8.6 "106 tests":** correctly treated as inventory-at-baseline;
  just reaffirming the receipt validator, not the number, is the contract.

---

## 6. Decision ledger — Fable column

| Decision | Fable recommendation |
|---|---|
| Initial ARM runtime | Linux ARM64 VM on the M3 Max, sized per R2 |
| ARM lane required or shadow-only | Shadow-only until dedicated hardware |
| Registry fallback in Milestone 2 | Yes, with the buildkitd http config note |
| Cache budgets and retention | Per-host arithmetic per Q5; floors before budgets |
| Two-shard initial topology | Yes — on non-production x86 if the audit permits |
| Hash partition or explicit manifest | Hash + union receipt now; duration-aware LPT v2 |
| Shadow and flake campaign lengths | 10 + 20, plus the 30-day auto-return tripwire |
| Shared fixture as separate effort | Yes, unconditionally separate |
| Legacy-lane retention after cutover | 30 days |

No provisioning, Docker lifecycle change, or deploy is authorized by this
review; per the plan's own §10, implementation starts only after Paul's
decisions land in the ledger. The one thing that should not wait for the
effort is the B1 release-pipeline fix — that is broken on `main` today.
