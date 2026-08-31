# M5b status — permanent Dolby Vision Profile 7 conversion

**Status:** implementation in progress · **Branch:**
`effort/playback-caps-v2-m5b` · **Updated:** 2026-08-30

Companion to [PLAYBACK-CAPS-V2-PLAN.md](PLAYBACK-CAPS-V2-PLAN.md), which owns
the contract, and [OPERATIONS.md](OPERATIONS.md), which will own the finished
operator procedure — this page records what has actually been built and
proved. A checked box means the named evidence exists on the branch; it is not
a forecast.

> This work can replace or delete an operator's only media copy. The source
> stays untouched until the replacement passes every verification gate and
> the source size and modification time still match the queued observation.

## Progress — one candidate, one complete qualification

| Stage | State | Evidence or next action |
|---|---|---|
| Isolated workspace | Complete | Fresh clone at `/private/tmp/plurx-m5b.lQcDxU/repo`; the user's existing checkouts are untouched |
| Integration line | Complete | One `effort/playback-caps-v2-m5b` branch from current `main`; development commits use `PLURX_EFFORT_COMMIT=1` |
| Store ledger | Complete | SQLite v39 and replicated v20 implement the same terminal ledger; queue/transition contracts, historical fixtures, and SQLite-to-hiqlite import coverage pass |
| Conversion worker | Not started | Add the six-stage temp-file pipeline, exact verification, and source fence without changing M5a files |
| Operator surface | Not started | Add library/file actions, progress, availability reasons, settings, and admin UI |
| Image and probes | Not started | Pin `dovi_tool` and MKVToolNix, then report exact boot capability |
| Focused evidence | Not started | Run conversion safety, both-store migration, API, UI, and operations contracts |
| Adversarial review | Not started | Independent correctness, destructive-media, cluster, and UI/API review passes after the PR opens |
| Complete qualification | Not started | Run once after review fixes on the frozen effort-to-main candidate; every promotion job and the exact-tree receipt must pass |
| Merge | Not started | Merge only while the qualified head and `main` base are unchanged |

## Guardrails — active throughout the build

- The M5a playback, copy-pipe, fragment-index, and transcode files named in the
  handoff are off limits. M5b operates before the existing scanner re-probes
  the replaced file, so it does not need a playback-path exception.
- The replacement lives in a temporary directory beside the source. A rename
  across filesystems is not atomic, so a global temp directory is unsafe.
- Unknown `dovi_tool info` output records a null enhancement-layer type. It
  never guesses MEL or FEL and never fails an otherwise valid conversion.
- A committed ledger row is terminal. Automatic scanning may discover the new
  Profile 8 file again, but it cannot re-queue destructive work.
- Missing or too-old tools disable the named feature before queue admission.
  Starting a job and discovering the dependency halfway through would leave a
  large, misleading temporary artifact beside the media.

## Decisions for review

1. **Use one effort branch and one PR into `main`.** M5b is one reviewable
   milestone, while its database, daemon, image, API, UI, and documentation
   surfaces are inseparable for safety. This preserves the handoff's one-PR
   requirement and uses the repository's complete main-promotion gate once.
2. **Keep the existing checkout read-only.** The user requested an independent
   clone, and the existing checkout also contains unrelated uncommitted work.
3. **Treat physical MEL/FEL and truncated-disc acceptance as operator
   evidence.** Automated tests will use controlled tool/probe fixtures for
   every destructive boundary; the PR will keep the real-media checks explicit
   because no redistributable 60–80 GB source belongs in the repository.

## Final evidence — fill only from the frozen tree

| Evidence | Result |
|---|---|
| Focused local regressions | Store ledger: targeted SQLite migration and queue tests; backend-neutral store contract; import unit suite; all-target hiqlite-store compile |
| Adversarial review findings | Pending |
| Main promotion gate | Pending |
| Qualification receipt | Pending |
| Merge commit | Pending |
