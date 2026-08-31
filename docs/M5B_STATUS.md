# M5b status — permanent Dolby Vision Profile 7 conversion

**Status:** candidate assembly in progress · **Branch:**
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
| Integration line | Refresh required | `main` moved after the first integration merge. Finish the active review fixes, merge the then-current `main`, refresh the Apple build claim if required, and review that exact tree before qualification |
| Store ledger | Complete | SQLite v40 and replicated v21 implement the same non-cascading replacement-guard lifecycle; direct and fenced SQLite/three-voter contracts, fail-closed migrations, and current/v14 import parity pass |
| Conversion worker | Complete | The destructive pipeline and crash-convergent cleanup pass 74 focused worker tests. Source-consuming tools inherit the manifest-bound descriptor instead of reopening a pathname; output must prove Profile 8.1 compatibility, explicit RPU presence, and no enhancement layer. Every absent guard/scratch decision requires a durable source-parent witness, terminal ledger deletion requires its `scratch_removed` tombstone, and the exact missing-mount regression proves all three cleanup boundaries fail closed |
| Operator surface | Complete | The admin guard projection is coherent and bounded. Operations names the filesystem witness, permanent terminal tombstone, missing-mount refusal, private cleanup anchor, dedicated-account trust boundary, and manual-removal prohibition. Shipped mounts remain read-only by default; Compose, systemd, Unraid, and Kubernetes now name the exact opt-in writable-library contract, while the interactive-user macOS LaunchAgent explicitly keeps conversion Off |
| Image and probes | Complete | Image installs checksum-pinned `dovi_tool` 2.3.3 assets and exact Bookworm MKVToolNix 74.0.0-1; the boot probe names command, version, availability, and an exact refusal reason |
| Focused evidence | Complete | The frozen attestation tree passes 74 worker tests, the focused admin API regression, the unavailable-tools/mount-disappearance state regression, production compile, strict daemon-test Clippy, rustfmt, and patch hygiene. SQLite and three-voter migration/import/queue contracts, the deterministic concurrent queue-outcome race, guarded rollback retirement, operations/package contracts, the Apple build 102 claim, and web checks also pass |
| Adversarial review | Complete | Severity-ordered passes found and fixed mount-loss retirement, reusable-path deletion, transient source-path tool reads, incomplete Profile 8.1 verification, legacy guardless claims, non-atomic replicated queue outcomes and upgrade trigger installation, guarded rollback cleanup, retry truth, API bounds, stale status claims, read-only deployment contradictions, the macOS same-uid trust violation, and launchd tool discovery. Three independent final verdicts are clean |
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
   continuing ledger reads. File detail loads at most 1,024 visible actions
   through no more than four sequential 256-ID reads, including
   server-computed eligibility and one shared tool-capability snapshot. Files
   beyond that cap show an explicit not-loaded state; the page never fans out
   one request per file or issues ledger reads concurrently.
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
8. **Persist a recovery guard when policy deletes the Profile 7 source.** No
   portable filesystem operation can prove a public pathname still names the
   verified replacement while deleting its last other hard link. Keep one
   same-filesystem hard link inside the conversion-owned hidden scratch, record
   it in a separate non-cascading guard ledger, and expose its state to
   administrators. The conversion stays `verified` until original retention or
   deletion is durable; only then can one fenced transaction activate the guard
   and mark the conversion `committed`. Confirmed catalog deletion leaves the
   guard row discoverable for bounded, leased orphan cleanup rather than
   orphaning media blocks under an untracked hidden path.
9. **Require mounted-filesystem evidence before cleanup accepts absence.** A
   deterministic scratch path can be missing because cleanup finished or
   because this voter sees the empty directory underneath a missing mount.
   Before the first destructive guard step, persist a bounded witness in the
   source parent while the owned scratch and proof link are both validated on
   that filesystem. Every later absent-path decision requires the same bound
   witness. After scratch removal, retain a tiny terminal tombstone before
   deleting the ledger row; permanent metadata is safer than either retaining
   a 60–80 GB inode or letting an unmounted voter erase the only ownership
   record.
10. **Retire files inside a dedicated-account private anchor.** macOS and Linux
    do not expose a portable compare-and-unlink syscall. Move the checked
    object into a daemon-owned, no-follow, same-filesystem `0700` directory and
    retire it only through the held directory capability while the Store file
    lease serializes plurxd workers. This protects against every other Unix
    uid and avoids retaining a full media inode. The explicit boundary is that
    arbitrary processes running as the plurxd uid are trusted; operations now
    requires a dedicated service account rather than sharing that uid with a
    downloader, organizer, or shell job.
11. **Bind tool inputs and verify the exact claimed output.** The long-running
    ffmpeg extraction and mkvmerge remux inherit a duplicated descriptor for
    the already manifest-bound source; they never reopen its reusable pathname,
    and the held inode is checked before and after each child. Original deletion
    also requires the replacement probe to report Profile 8, compatibility id
    1, explicit RPU presence, and no enhancement layer. A generic or incomplete
    Profile 8 output is not Profile 8.1 and fails closed.

## Final evidence — fill only from the frozen tree

| Evidence | Result |
|---|---|
| Focused local regressions | `cargo test -p plurxd dv_disk::tests:: --no-fail-fast` (74/74); guarded rollback plus unavailable-tools/mount-disappearance scheduler regression (1/1); focused admin API; SQLite and three-voter Store/migration/import contracts plus the concurrent queue-outcome race; `make operations-check`; `make web-check`; `make apple-build-bump`; `cargo check -p plurxd --bin plurxd`; `cargo clippy -p plurxd --bin plurxd --tests -- -D warnings`; `cargo fmt --all -- --check`; `git diff --check` |
| Adversarial review findings | Fixed cleanup lease/slot races, exact-object identity and portable rebaseline persistence, descriptor-bound tool inputs, exact Profile 8.1/RPU verification, bounded residual filesystem I/O and tool/probe output, proof-creation/removal namespace races, private-anchor retirement, failure-cleanup fencing, durable parent syncs, truthful guarded commit, non-cascading orphan lifecycle, missing-tool cleanup scheduling, mounted-filesystem absence attestation, legacy guardless claims, atomic replicated queue outcomes and upgrade trigger installation, guarded rollback convergence, row-id reuse, missing-link fail-closed projection, migration/import failures, retry eligibility truth, UI/API request bounds, operator visibility, read-only packaging contradictions, macOS service-account safety, launchd tool discovery, and stale documentation. Three final read-only verdicts are clean |
| Main promotion gate | Pending |
| Qualification receipt | Pending |
| Merge commit | Pending |
