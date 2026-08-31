# M5b status — permanent Dolby Vision Profile 7 conversion

**Status:** final exact-tree review pending · **Branch:**
`effort/playback-caps-v2-m5b` · **PR:**
[#710](https://github.com/pjunod/plurx/pull/710) · **Updated:** 2026-08-31

Companion to [PLAYBACK-CAPS-V2-PLAN.md](PLAYBACK-CAPS-V2-PLAN.md), which owns
the contract, and [OPERATIONS.md](OPERATIONS.md), which will own the finished
operator procedure — this page records what has actually been built and
proved. A checked box means the named evidence exists on the branch; it is not
a forecast.

> This work can replace or delete an operator's only media copy. The source
> stays untouched until the replacement passes every verification gate and
> the worker confirms that its open source still matches the scanner's
> size/mtime identity and the exact storage-object identity captured after the
> row is leased, before the long conversion begins.

## Progress — one candidate, one complete qualification

| Stage | State | Evidence or next action |
|---|---|---|
| Isolated workspace | Complete | Fresh clone at `/private/tmp/plurx-m5b.lQcDxU/repo`; the user's existing checkouts are untouched |
| Integration line | Complete | Current `main` at `623512ea` is merged into `effort/playback-caps-v2-m5b`; Apple build 98 is claimed; development commits use `PLURX_EFFORT_COMMIT=1` |
| Store ledger | Complete | SQLite v39 and replicated v20 implement the same terminal ledger; queue/transition contracts, historical fixtures, and SQLite-to-hiqlite import coverage pass |
| Conversion worker | Complete | Six-stage sibling-temp pipeline, exact mux verification, scanner-identity plus open-handle source fence, leased parallel queue, fenced state transitions, and crash recovery compile and pass focused unit tests |
| Operator surface | Complete | Admin-only library modes, progress, settings, file action/ledger, explicit tool refusal, and retry controls pass the embedded-web static contract |
| Image and probes | Complete | Image installs checksum-pinned `dovi_tool` 2.3.3 assets and exact Bookworm MKVToolNix 74.0.0-1; the boot probe names command, version, availability, and an exact refusal reason |
| Focused evidence | Complete | Conversion and API unit tests, SQLite and replicated Store contracts, both backend compilation, embedded-web static contracts, 60-layout/view UI capture, validation catalog, and 127 operations contracts pass |
| Adversarial review | In progress | Three independent first-pass reviews and the deeper remediation pass are complete; destructive-media, clustered-store, API/UI, documentation, and packaging findings are fixed. Review the exact merged candidate before qualification |
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
4. **Require an explicit enhancement layer and RPU at admission.** Numeric
   Profile 7 and HDR10 compatibility remain the primary eligibility contract,
   but a destructive rewrite also refuses an incomplete or not-yet-backfilled
   probe row. A later successful scan makes it eligible without an exception.
5. **Poll conversion state only while durable work is active.** The Libraries
   tab and an admin file detail load one bounded snapshot, then refresh at most
   once every ten seconds while a row is `queued`, `running`, or `verified`.
   The first all-terminal snapshot stops the timer, so an idle page spends no
   continuing ledger reads. File detail uses one batch read for all visible
   files, including server-computed eligibility and one shared tool-capability
   snapshot; it never fans out one request per file.
6. **Do not invent ledger history while importing an old backup.** SQLite v14
   predates the v39 conversion table, so its migration creates an empty ledger;
   a current-schema import separately proves that a real failed conversion row
   and its audit error survive SQLite-to-hiqlite activation.
7. **Bound every admission mutation.** A manual or automatic library pass
   admits at most 64 rows in one Store transaction. `saturated` is deliberately
   a cap-hit hint rather than a racy remaining-work count; Settings tells the
   operator to run another bounded pass, while automatic mode advances one
   library per leased tick. Admin batch reads are separately capped at 256
   file IDs.

## Final evidence — fill only from the frozen tree

| Evidence | Result |
|---|---|
| Focused local regressions | Worker safety: all 30 `dv_disk` tests pass. Settings: all 8 focused tests pass. Store parity: the SQLite and three-voter conversion contracts pass, including bounded retry fairness and committed-cleanup cursor selection. `cargo check -p plurx-core --all-targets --features hiqlite-store`, workspace compile, focused clippy, `make web-check`, and all 127 operations contracts pass |
| Adversarial review findings | The first pass fixed crash-publication, no-clobber/source-swap, scratch ownership, durability, MKV eligibility, lease-loss cancellation, unbounded admission, replicated queue races, migration strictness, N+1/batch-read, concurrent mode update, settings partial-write, probe timeout/version, API 404, polling/accessibility, documentation, and arm64-build gaps. The deeper pass additionally bound recovery and rollback to verified storage identities, preserved the queued retention policy, made safe rollback retryable only after proof, added crash-convergent cleanup, prioritized unseen rows over retries, prevalidated the whole settings request, and decoupled successful UI mutations from refresh reads. Exact merged-tree review pending |
| Main promotion gate | Pending |
| Qualification receipt | Pending |
| Merge commit | Pending |
