# M7 R-M3 handoff — finish M2, then build seek coalescing

**Status:** M1 merged · M2 PR #789 qualifying · M3 not started ·
**Audience:** Claude or another implementation agent · **Written:** 2026-09-02

Companion to
[M7-REMAINDER-HANDOFF.md](M7-REMAINDER-HANDOFF.md),
[PLAYBACK-CONTROL-PROTOCOL-PLAN.md](PLAYBACK-CONTROL-PROTOCOL-PLAN.md)
§13.8, and the adjudicated M1–M3 remediation brief pasted into the originating
Codex task. Read those sources in that order before changing code. This file
records the live execution state and the exact remaining work; it does not
replace their contracts.

Work strictly in order: finish and merge R-M2, then start R-M3. Do not begin
M3 while M2 acceptance is pending, stale, or red. If any design cannot meet
the original §8.1 acceptance language or the two explicit remediation rulings,
stop and name the contradiction instead of weakening the criterion.

## 1. Objective — close M2 and M3 without handing merges to Paul

The remaining executor owns both implementation and merge:

1. Wait for PR #789's exact candidate to pass `Main promotion gate`.
2. Reconfirm that the candidate contains the current `main` tip.
3. Merge #789 with a merge commit. Paul must not have to merge it.
4. Create R-M3 from the resulting current `main`.
5. Implement the seek-coalescing contract in §7–§9 below.
6. Prove the focused and full acceptance, open one substantive PR, and merge
   it with a merge commit after its current-candidate promotion gate passes.

No deploy is part of this handoff. M4 burn-join is being implemented
separately, and marker prewarm belongs exclusively to
[MARKER-PREWARM-HANDOFF.md](MARKER-PREWARM-HANDOFF.md).

## 2. Live state — re-read GitHub before acting

These facts were verified on 2026-09-02. They are orientation, not permission
to trust stale state.

| Item | Verified state |
|---|---|
| R-M1 | PR #775 merged by the executor |
| R-M1 merge commit | `5dc390d9488114703620aa61e085fb161b8381e2` |
| R-M2 PR | `https://github.com/pjunod/plurx/pull/789` |
| R-M2 branch | `codex/m7-r-m2` |
| R-M2 worktree | `/private/tmp/plurx-m7-r-m2` |
| R-M2 exact head | `7e0024605fca2ec11ec4bda934d3b97c89eb2799` |
| Integrated `main` | `eb6c82706c4acb849ee61f64087cae6ea3683066` |
| Current CI run | `33637026934`, attempt 2, exact head above |
| CI runner mode | repository variable restored to `self-hosted` |
| R-M3 | not started, by design |

The prior attempt of run `33637026934` was canceled after Android
instrumentation failed before tests: repository variable `CI_RUNNER_MODE` was
`github`, the hosted Ubuntu worker had no `/dev/kvm`, and the workflow stopped
at `test -c /dev/kvm`. That was an incompatible runner, not an Android test
failure. The same commit is now rerunning as attempt 2 in documented
`self-hosted` mode.

The only uniquely labeled Store runner is `gha-nuc1-general-01`. It was
offline, then returned online on 2026-09-02. Do not bypass its required Store
job. The repository deliberately uses `ci-store` to keep this heavy stateful
suite off production voters.

### 2.1 R-M2 conflict and exact-tree evidence

Current `main` was merged into R-M2 with merge commit `7e002460`. The only
content conflict was
`tests/playback/rolling-producer-owners.toml`. Both branches' ownership
comments were preserved, and the combined structural count was measured and
set to `200`. This focused proof passed:

```bash
python3 -m unittest \
  tests.validation.test_rolling_producer_ownership_inventory
# 7 tests, 0 failures
```

The exact committed tree at `7e002460` then passed the complete local gate
with pinned Rust 1.97.1:

```bash
env PATH=/Users/pjunod/.cache/codex-runtimes/codex-primary-runtime/dependencies/bin/fallback:/Users/pjunod/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin \
  make check
```

Observed results: validation catalog 23 points · 27 checks · 10,406 audited
files; history 1,223 corrective commits · 852 direct test changes · 726
current-check mappings · 93 client anchors · 86 non-runtime corrections;
operations 156/156; validation 52/52; `cargo fmt --check`; clippy with
`-D warnings`; full workspace green, including 1,564 `plurxd` tests with zero
failures and three intentional ignores. The only compiler output was the
known compact-unwind linker warning.

Two untracked build caches exist in the M2 worktree:

```text
vendor/hiqlite/target/
vendor/hiqlite-wal/target/
```

They are build products. Do not add, delete, clean, or move them.

## 3. Non-negotiable contract — no convenient reinterpretations

The original acceptance from M7 §8.1 remains binding:

> a 20-seek storm starts work only for the settled target; subtitle work
> never replaces video independently; no advertised subtitle interval
> returns an avoidable 404

The remediation makes the first clause physically testable rather than
clairvoyant:

- A create already known stale is refused with the existing typed 409 before
  any producer spawn.
- Work learned stale after it begins is torn down at supersession.
- A newer activation fences its predecessor's producer.
- An abandoned subtitle-window extraction aborts before publication.
- After settling, exactly one final-target producer survives.
- At no time does one playback own more than one live subtitle-window flight.

All of these must be true together. Nineteen logical `supersedes` verdicts
alone are not acceptance.

The following guardrails are equally binding:

- Add no detectors and no recovery opinions.
- Subtitle work never advances or replaces the video pointer.
- Keep the empty `WEBVTT\n\n` fallback and its `no-store` behavior.
- Preserve the `media_origin_seconds` cue-time shift.
- Keep the whole-track warm authoritative and unchanged in purpose.
- Do not resurrect the disabled VOD seek-storm browser case.
- Do not touch M4 burn-join or marker prewarm.
- Do not deploy.
- Do not open a PR merely to track status. One substantive PR per milestone.
- Merge with merge commits only; never squash or rebase the milestone history.

## 4. Finish R-M2 — exact commands and stop conditions

Work from `/private/tmp/plurx-m7-r-m2`. The macOS system Git has crashed in
`libiconv` on this machine; use the bundled Git first on `PATH`:

```bash
export PATH=/Users/pjunod/.cache/codex-runtimes/codex-primary-runtime/dependencies/bin/fallback:/usr/bin:/bin
```

Poll the current attempt without dumping every successful job:

```bash
gh run view 33637026934 --repo pjunod/plurx \
  --json status,conclusion,attempt,headSha,jobs \
  | jq -c '{status,conclusion,attempt,headSha,jobs:[.jobs[]
      | select(.status != "completed" or
          (.conclusion != "success" and .conclusion != "skipped"))
      | {name,status,conclusion}]}'
```

When the run is green, re-read both refs before merging:

```bash
git fetch origin main
git merge-base --is-ancestor origin/main HEAD
git diff --check
gh pr view 789 --repo pjunod/plurx \
  --json state,mergeable,mergeStateStatus,baseRefOid,headRefOid,statusCheckRollup,url
```

If `origin/main` is not an ancestor, stop the merge attempt. Merge current
`origin/main` into `codex/m7-r-m2`, resolve only the actual conflicts, rerun
the full exact-tree local gate, push, and wait for the new exact CI candidate.
A green run for an older head is not evidence for the moved branch.

Only after `Main promotion gate` is green on the current candidate:

```bash
gh pr merge 789 --repo pjunod/plurx --merge
gh pr view 789 --repo pjunod/plurx --json state,mergedAt,mergeCommit,url
git fetch origin main
```

Record the returned merge commit in the R-M3 PR body. Do not leave this merge
for Paul.

### 4.1 The local hook re-entrancy trap

The merge commit's pre-commit hook invoked `make check`. During that gate,
`test_candidate_packager_binds_export_to_exact_source_tree` created a scratch
Git repository, but its fixture commit inherited the outer hook environment
and recursively invoked Plurx's hook in a directory without a `check` target.
The direct exact-tree `make check` passed afterward.

For a merge metadata commit only, `.git/hooks/pre-commit` explicitly documents
`--no-verify`. Using it is acceptable only when the identical committed tree
is immediately proved by direct `make check`; it is not permission to skip the
gate. Do not fold an unrelated release-test refactor into M2 or M3 merely to
silence this environmental recursion.

## 5. Start R-M3 — only after #789 is merged

Fetch the exact merge result, then create a fresh milestone worktree and
branch from it:

```bash
git fetch origin main
git worktree add -b codex/m7-r-m3 \
  /private/tmp/plurx-m7-r-m3 origin/main
cd /private/tmp/plurx-m7-r-m3
/Users/pjunod/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin/rustc --version
```

The version must be Rust 1.97.1 before any Rust edit. If that compiler loop is
not usable, follow
[AGENT-COMPILE-LOOP.md](AGENT-COMPILE-LOOP.md) and
[DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md); do not use CI as a
compiler.

Do not reuse the R-M2 worktree or branch for M3. The separate branch and PR
are part of the milestone contract, not bookkeeping.

## 6. Current code contracts — re-verify after #789 merges

The following locations were verified on R-M2 head `7e002460`. Line numbers
will move; names and behavior are the contract.

| Contract | Current location and shape |
|---|---|
| Settled authority | `playback_control.rs`: `SettledTarget { sequence, anchor_ms }` |
| Supersession slack | `SETTLED_TARGET_SLACK_MS = 1_000` |
| Verdict | `SettledTarget::supersedes(sequence, anchor_ms)` requires newer sequence and target difference beyond slack |
| Session lookup | `TranscodeManager::settled_target_for_session` returns `None` for missing, retired, or never-exchanged sessions |
| Whole/window flight registry | `subtitles.rs`: `Extraction { result, ready }`, global `extractions()` |
| Exact-span warmer dedup | `subtitles.rs`: global `warmups(): Mutex<HashSet<PathBuf>>` |
| Window entry point | `warm_vtt_window` delegates to injected `warm_vtt_window_with` |
| Actual extraction | `ensure_vtt_at` starts a detached owner task and atomically publishes through `publish_extraction` |
| Child safety | ffmpeg commands use `kill_on_drop(true)` |
| HLS seam | `SubtitleSegmentSource::warm_window` currently receives file/index/anchor/span, not session authority |
| HLS fallback | `subtitle_vtt_local_before_with_source` returns `WEBVTT\n\n` with `no-store` after starting best-effort warms |
| Rolling lifecycle | `own_rolling_retirement` owns terminal admission, exact reap, registry removal, and resource release |
| VOD lifecycle | `Rendition::detach_reader(session_id)` removes the reader and marks the rendition dormant |

Important consequence: canceling only the outer task created by
`warm_vtt_window_with` does not cancel the actual extraction, because
`ensure_vtt_at` currently detaches another owner task. R-M3 must refactor the
window path so the per-session owner controls the real unpublished extraction.
The whole-track path must retain its current detached, cancellation-independent
contract.

## 7. R-M3 implementation — one per-session window owner

Add one owner registry keyed by playback session id. Each session slot holds:

- the authoritative control sequence, when one exists;
- the grid-aligned window anchor and span;
- an abort/cancellation handle for the actual extraction;
- completion state sufficient to wait for teardown before starting a
  replacement.

The exact data types may follow the surrounding Tokio style, but the state
machine is fixed:

```text
segment request
      │
      ▼
compute demand anchor ──▶ read settled target immediately before warm
      │
      ├─ target exists, window misses target beyond 1 s slack
      │       └─▶ start nothing; serve existing empty fallback
      │
      ├─ no slot or settled previous slot
      │       └─▶ start one owned actual extraction
      │
      ├─ same anchor
      │       └─▶ join; never spawn a second extractor
      │
      ├─ different anchor + strictly later authority
      │       └─▶ abort old unpublished flight; await settlement; start new
      │
      └─ different anchor without later authority
              └─▶ keep old flight; start nothing
```

Waiting for the canceled flight to settle before spawning its successor is
what proves the physical maximum of one live window producer. Merely dropping
a handle and starting immediately can overlap two ffmpeg children.

### 7.1 Cancellation and publication are different outcomes

Refactor only the window extraction path to distinguish completion from
abortion. A useful internal shape is an outcome like:

```rust
enum WindowExtractionOutcome {
    Complete(Result<PathBuf, String>),
    Aborted,
}
```

The names are optional; the properties are not:

- Abortion drops the real extractor future, so `kill_on_drop(true)` kills the
  unpublished ffmpeg child.
- Abortion unlinks its temporary file, clears `extractions()` and `warmups()`,
  notifies joiners, and creates no negative memo.
- A genuine extraction error retains the existing bounded negative memo.
- Once extraction succeeds, validation and atomic publication finish without
  a cancellation point that can strand or half-publish the sidecar.
- A published sidecar is never killed or removed by window-flight
  cancellation. It retires only through existing whole-track supersession or
  budget pruning.
- The whole-track `ensure_vtt` remains detached and cancellation-independent.

Keep global exact-path `warmups()` as cross-session dedup underneath the new
per-session slot. The new owner bounds one playback; the existing registry
prevents different playbacks requesting the identical span from duplicating
work.

### 7.2 HLS admission uses the settled target

Extend `SubtitleSegmentSource::warm_window` and the production implementation
with the session id and settled authority needed by the owner. In
`subtitle_vtt_local_before_with_source`:

1. Compute the anchor exactly as today from
   `media_origin_seconds + segment_start` and `window_anchor_seconds`.
2. Immediately before warming, call
   `state.transcode.settled_target_for_session(session).await`.
3. If authority exists and the requested window does not cover its target
   beyond the existing 1 s slack, skip the warm and return the unchanged empty
   fallback.
4. If authority is absent, allow first-play warming. `None` means there is no
   ordering fact, not that work is stale.

Put the coverage comparison on `SettledTarget` or beside its existing slack so
there is one definition of the 1 s tolerance. Do not duplicate a magic `1_000`
inside subtitles or HLS.

### 7.3 Session end releases the owner

Add a cancellation-safe `release_session_window(session_id)` operation that
removes/closes the slot, aborts any unpublished flight, and waits until the
actual producer settles.

Wire it into both lifecycle families:

- Rolling: after terminal admission in `own_rolling_retirement`, covering the
  requested session id and the registered key if adoption renamed it.
- VOD: every path already converging through `Rendition::detach_reader`.
  Remove the reader, drop the readers lock, then await subtitle-owner release;
  never hold the reader mutex across the await.

Recheck idle, terminal, resurrection, and reattachment paths so none bypasses
`detach_reader`. A slot that survives session end would make a later same-id
session inherit obsolete authority.

## 8. R-M3 storm test — production facts, not verdict counts

Add a test module whose name includes `seek_coalescing` so the required filter
selects a non-zero inventory. The main fixture is HTTP/protocol-level, not the
disabled VOD browser storm.

Drive 20 snapshot/create pairs spaced more than the protocol's 250 ms exchange
floor. Instrument producer spawn, activation fencing/teardown, and window
flight concurrency. Force at least one ordering where snapshot `N+1` is
accepted before create `N` reaches admission.

The test must assert all of these observable facts:

1. The deliberately stale create returns the existing typed 409 before spawn.
2. Equal and lower control sequences remain rejected exactly as before.
3. Each accepted successor activation fences its predecessor through the
   existing `fence_predecessor` path.
4. One playback never has more than one live window producer.
5. An obsolete unpublished window aborts and publishes nothing.
6. A successfully published window is never removed mid-write.
7. A first-play session with no settled target still warms.
8. After the storm settles, exactly one session producer remains live and it
   carries the final target.

Also retain deterministic focused tests for one session traversing at least
three anchors, same-anchor joining, different-anchor refusal without newer
authority, and slot cleanup at session end.

Do not rename a set of logical `supersedes` unit assertions and call that the
storm. Acceptance is at the production boundary.

## 9. Validation — every named proof must be green

Develop with the smallest focused loop, but finish on the exact committed,
current-main tree:

```bash
cargo test -p plurxd seek_coalescing
# Must report a non-zero test count.

cargo test -p plurxd subtitles
cargo test -p plurxd playback_control
python3 -m unittest tests.validation.test_rolling_producer_ownership_inventory
git diff --check
make history-check
make check
```

If browser-facing fixtures or contracts change, also run the relevant
`node --test tests/playback` inventory. The PR must keep steady VOD and
suspend/resume browser acceptance green. The disabled VOD seek-storm case
must remain disabled and untouched.

M3 is expected to be server-only. Do not edit native client sources merely to
force a build bump. If a real requirement does touch
`clients/android/app/src/main/` or `clients/apple/Sources/`, use the generated
five-surface bump workflow and clear current `main`; never hand-edit one
version surface.

Update `validation/points.toml` for every changed `crates/**` surface and keep
[PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md) honest in the same
commit. A fix-shaped commit subject needs the repository's corresponding
`validation/regressions.d/` history evidence.

## 10. R-M3 PR and merge — substantive, current, executor-owned

Open the PR only after the focused tests and exact local gate are green. It is
the implementation PR, not a tracking PR. Its body should record:

- R-M2 merge commit used as its base;
- exact R-M3 head SHA;
- changed ownership and cancellation boundaries;
- the non-zero `seek_coalescing` count;
- focused command results;
- exact `make check` result;
- confirmation that the disabled VOD storm, M4, prewarm, clients, and deploys
  were untouched.

Before merging, freeze the candidate and repeat the same current-base audit as
§4. If `main` moves, merge it into the branch with a merge commit and
requalify the exact new tree. Then:

```bash
gh pr merge <R-M3-PR> --repo pjunod/plurx --merge
gh pr view <R-M3-PR> --repo pjunod/plurx \
  --json state,mergedAt,mergeCommit,statusCheckRollup,url
git fetch origin main
```

Do not ask Paul to merge it. The executor owns the merge once the current
candidate's `Main promotion gate` and qualification receipt are green.

## 11. Stop conditions — name the exact broken criterion

Stop and report rather than improvise if any of these becomes true:

- The design cannot keep one physical live window producer per playback.
- Cancellation cannot reach the actual unpublished extractor without also
  making whole-track publication request-owned.
- Session cleanup cannot release the owner without holding an unrelated
  lifecycle lock across an await.
- A stale-window refusal would require removing or caching the empty fallback.
- The storm can prove logical supersession but cannot observe spawn and reap.
- M3 would need subtitle work to replace or advance video.
- Current `main` cannot be merged without weakening an earlier acceptance.

When reporting, cite the exact bullet and the concrete code conflict. "Too
hard" is not a stop condition; violating the contract is.

## 12. Quick resumption checklist

```bash
cd /private/tmp/plurx-m7-r-m2
export PATH=/Users/pjunod/.cache/codex-runtimes/codex-primary-runtime/dependencies/bin/fallback:/usr/bin:/bin

git status -sb
gh variable get CI_RUNNER_MODE --repo pjunod/plurx
gh run view 33637026934 --repo pjunod/plurx \
  --json status,conclusion,attempt,headSha,jobs
gh pr view 789 --repo pjunod/plurx \
  --json state,mergeable,mergeStateStatus,baseRefOid,headRefOid,statusCheckRollup,url
gh api repos/pjunod/plurx/branches/main --jq .commit.sha
```

The correct immediate next action is determined by those five outputs:
wait/fix CI, merge a moved `main` and requalify, or merge #789. Only after
#789 is actually merged does R-M3 begin.
