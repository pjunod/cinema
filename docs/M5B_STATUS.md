# M5b status — permanent Dolby Vision Profile 7 conversion

**Status:** implementation in progress · **Branch:**
`effort/playback-caps-v2-m5b` · **Updated:** 2026-08-31

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
| Conversion worker | Complete | Six-stage sibling-temp pipeline, exact mux verification, scanner-identity plus open-handle source fence, leased parallel queue, fenced state transitions, and crash recovery compile and pass focused unit tests |
| Operator surface | Complete | Admin-only library modes, progress, settings, file action/ledger, explicit tool refusal, and retry controls pass the embedded-web static contract |
| Image and probes | Complete | Image installs checksum-pinned `dovi_tool` 2.3.3 assets and exact Bookworm MKVToolNix 74.0.0-1; the boot probe names command, version, availability, and an exact refusal reason |
| Focused evidence | In progress | Conversion verification unit tests, SQLite Store contract, both backend compilation, embedded-web static contract, validation catalog, and 127 operations contracts pass; replicated Store, UI structure, and API evidence remain |
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
4. **Require an explicit enhancement layer and RPU at admission.** Numeric
   Profile 7 and HDR10 compatibility remain the primary eligibility contract,
   but a destructive rewrite also refuses an incomplete or not-yet-backfilled
   probe row. A later successful scan makes it eligible without an exception.
5. **Do not continuously poll conversion state from Settings.** The Libraries
   tab loads one admin snapshot, refreshes it after each mutation, and the
   ordinary page refresh obtains a new one. This keeps the established bounded
   settings read contract while still exposing per-library progress.

## Final evidence — fill only from the frozen tree

| Evidence | Result |
|---|---|
| Focused local regressions | Store ledger: targeted SQLite migration and queue tests; backend-neutral Store contract; import unit suite; all-target hiqlite-store compile. Worker/operator surface: five conversion verification/capability unit tests; `cargo check -p plurxd --all-targets`; `make web-check`. Packaging: 127 operations contracts, including exact tool pins and both release-asset checksums |
| Adversarial review findings | Pending |
| Main promotion gate | Pending |
| Qualification receipt | Pending |
| Merge commit | Pending |
