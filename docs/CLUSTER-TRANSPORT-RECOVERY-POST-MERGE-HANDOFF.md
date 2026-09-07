# Cluster transport recovery post-merge handoff — finish qualification and rollout

**Status:** ready after deploy-first promotion · **Owner:** next operator ·
**Written:** 2026-09-06

Companion to
[CLUSTER_TRANSPORT_RECOVERY_STATUS.md](CLUSTER_TRANSPORT_RECOVERY_STATUS.md),
[DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md), and
[OPERATIONS.md](OPERATIONS.md) §19. This document is the exact remaining work
after the recovery implementation is merged early for runtime use. It does not
claim the deferred evidence already exists.

## 1. Objective — convert deploy-first confidence into retained qualification

Freeze one current `main` commit, run the complete recovery campaign and full
suite against that same tree, retain the receipts, then verify the published
image and fleet. If `main` moves, start again from the new tree; evidence from
an older SHA is diagnostic only.

The shipped confidence boundary is deliberately smaller: Rust 1.97.1 compile,
Clippy, 27 focused recovery tests, operations/history contracts, three clean
adversarial reviews, one unmeasured voter warmup, and four consecutive counted
large-image voter recoveries. Paul chose that boundary so the implementation
could run before the remaining minutiae finished.

## 2. Remaining evidence — do not collapse these into the smoke result

| Evidence | State at handoff | Completion signal |
|---|---|---|
| Voter smoke | four counted cycles passed after one warmup | confidence only; never a qualification receipt |
| Full recovery campaign | not run to completion | 20 voter and 20 learner cycles in the retained closed-schema artifact |
| Full repository suite | deferred | every required Main promotion surface succeeds on one frozen tree |
| Qualification receipt | absent | a first-attempt main-bound qualification PR binds the exact tested SHA/tree and records every required job as successful |
| Main image | pending promotion/publication | immutable SHA image and the verified `main` alias exist in the Forgejo registry |
| Fleet rollout | not verified | every intended voter reports the promoted build SHA and healthy cluster state |

## 3. Freeze the tree — work from an independent clone

Do not use Paul's working checkout. Create a fresh clone, fetch `main`, and
record both commit and tree before running anything:

```bash
git clone ssh://git@192.168.4.7:222/noirr/plurx.git plurx-recovery-qualification
cd plurx-recovery-qualification
git switch main
git pull --ff-only
git rev-parse HEAD
git rev-parse 'HEAD^{tree}'
/Users/pjunod/.cargo/bin/rustc +1.97.1 --version
```

Expected compiler: Rust 1.97.1. If qualification runs on another node, send a
`git archive` of the frozen commit. Transfer neither `.git` nor credentials.
The approved node key is
`/Users/pjunod/code/plurx-agent/.ssh-deploy-key`.

## 4. Run the deferred 20+20 campaign — exact SHA, retained bytes

The campaign intentionally runs voter first, then learner. It performs one
unmeasured full warmup per role before fixing the zero-margin resource baseline.
Every measured cycle must return to that baseline within the absolute
60-second cleanup horizon.

```bash
candidate_sha="$(git rev-parse HEAD)"
PLURX_BUILD_SHA="$candidate_sha" make cluster-transport-recovery-check
```

The local command writes the closed-schema evidence only:

```text
target/validation/cluster-transport-recovery.json
```

The CI lane wraps that same command with `tee`, validates the evidence, and
retains all three lane outputs:

```text
target/validation/cluster-transport-recovery.log
target/validation/cluster-transport-recovery.json
target/validation/cluster-transport-recovery-receipt.json
```

The JSON must pass the Rust closed-schema and semantic validator. The lane
receipt binds the exact bytes, SHA, tree, lane command, and workflow attempt.
Do not claim that a local `make` invocation creates the log or receipt, and
never substitute the voter smoke output: it deliberately creates no retained
qualification artifact.

## 5. Run the full suite once — same frozen tree

Use the repository's complete Main promotion fan-out, not only `make check`.
Require successful Rust, cluster store/topology/recovery/WAL/daemon, web/VOD,
Android, Apple, package-smoke, cross-build, and aggregate promotion results.
The current workflow emits the aggregate `qualification-receipt.json` only for
a qualifying main-bound pull request; a post-merge `main` push does not create
that artifact. The deploy-first merge therefore leaves the aggregate receipt
open even when the post-merge fan-out and `publish_main` succeed. Close that
gap with a first-attempt qualifying PR bound to the exact candidate, or add a
truthful post-merge aggregate receipt path before claiming receipt completion.

If a test fails, fix the cause in a proper `codex/` branch and PR, run its
smallest focused regression until green, obtain three adversarial reviews, and
then run one new complete qualification on the corrected frozen tree. Do not
reuse green jobs or receipts from the failed candidate.

## 6. Verify publication and rollout — prove what is actually running

The main-push workflow publishes the fleet image only after its post-merge
fan-out. Confirm the registry contains the immutable SHA and that the `main`
alias resolves to it. Then use the normal fleet deployment workflow and verify
each intended voter independently:

- reported build SHA equals the promoted `main` SHA;
- cluster membership and quorum are healthy;
- transport status has no active/stalled orphan from rollout;
- a settings write and watch-state write replicate across voters;
- one controlled node restart rejoins without data loss.

Record exact commands, timestamps, node results, image digest, and rollback
trigger in the status page. Do not infer fleet completion from one healthy
container.

## 7. Guardrails — preserve the shipped contract

- Do not add a production feature gate. The Dev settings enablement section is
  informational and must explain prerequisites for safe operation.
- Do not weaken readiness, Raft authority, snapshot identity/offset checks,
  zero resource margins, or absolute transfer/install/cleanup deadlines.
- Do not change the production snapshot threshold to accelerate validation;
  only the harness-owned validation launch may force snapshots.
- Do not send repository credentials or `.git` to compiler or test nodes.
- Do not call three voter cycles, a green effort gate, or a rerun attempt full
  qualification.

## 8. Acceptance — close this handoff only with all boxes true

- [ ] Current `main` SHA and tree are frozen and recorded.
- [ ] 20 voter and 20 learner measured recoveries pass on that exact SHA.
- [ ] Closed-schema evidence and lane receipt are retained.
- [ ] The full Main promotion suite passes once on the same frozen tree.
- [ ] An aggregate qualification receipt is retained from a first-attempt
      main-bound qualification run, or the workflow has gained and used an
      equivalent post-merge exact-tree receipt path.
- [ ] Three adversarial reviews cover any corrective PR raised afterward.
- [ ] Immutable image digest and `main` alias are verified.
- [ ] Every intended voter reports the promoted SHA and healthy state.
- [ ] Controlled restart and replicated settings/watch-state checks pass.
- [ ] `CLUSTER_TRANSPORT_RECOVERY_STATUS.md` records links and exact evidence.

Until every item is checked, the implementation may be running, but exhaustive
qualification and rollout verification remain open work.
