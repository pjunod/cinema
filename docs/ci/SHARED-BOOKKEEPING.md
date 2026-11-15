# Shared bookkeeping — one file per change, so pull requests never meet

**Status:** live. Owners: the validators named per section. Added 2026-11-15.

Companion to [PAPERWORK.md](PAPERWORK.md) (which rules are checked, and when)
— this is *where a change records itself*, and what an open pull request does
after this layout landed.

Six files used to be edited by nearly every pull request at the same line,
so merging `main` into a branch conflicted in them almost every time. Each
now takes a file per change, or takes nothing at all. Two pull requests write
two paths; Git has nothing to merge. No merge driver or `.gitattributes
merge=` rule is involved, so Forgejo's conflict check and the merge train
need no special handling.

| You are recording | Write this | Never edit |
|---|---|---|
| A changelog entry | `changelog.d/<slug>.md` | `CHANGELOG.md` `[Unreleased]` |
| A client change | nothing — no build number, no status line | `versionCode`, `CURRENT_PROJECT_VERSION`, the README status lines |
| A changed task/timer/process count | `tests/rolling-producer-owners.d/<slug>.toml` | `expected_occurrences` in `tests/playback/rolling-producer-owners.toml` |
| A client fix anchor | `tests/client-fixes.d/<slug>.toml` | new rows in `tests/client-fixes.toml` |
| A file under an existing functionality point | `validation/points.d/<slug>.toml` | that point's `paths` in `validation/points.toml` |
| An effort's status | `status.d/<YYYY-MM-DD>-<slug>.md` | `STATUS.md` |

Name the file after the change (the branch or the subject), not after a
number that another branch could also pick.

## Changelog (`changelog.d/`)

A fragment is the text that would have gone under `[Unreleased]`:

```markdown
### Fixed

- A leader restart no longer ends Live TV playback.
```

Headings are `Added` · `Changed` · `Deprecated` · `Removed` · `Fixed` ·
`Security`. `scripts/release-cut` folds every fragment into the dated section
— by category, fragments in file-name order, then whatever `[Unreleased]`
still held — and deletes them, so no entry is lost and the result does not
depend on merge order. A release commit that still carries a fragment is
refused. Preview the next section with
`python3 -m validation.changelog_fragments --preview`.
Format: [changelog.d/README.md](../../changelog.d/README.md).

## Store build numbers

**No branch claims a build number.** `build.gradle.kts` and `project.yml`
carry the development build, `1`, and nothing edits it. Each branch used to
write "the next number", so any two client branches claimed the same one and
the second to merge re-claimed it.

A store build is numbered when it is released, from the commit it is built
from: **the count of commits that commit contains**
(`make mobile-build-number`, [validation/mobile_build.py](../../validation/mobile_build.py)).
A later `main` always contains more commits, so the number rises with every
release; the same commit always gets the same number; Apple and Android share
it. The last hand-claimed numbers were Apple 301 and Android 207; the first
assigned one is above 15,000.

A commit that is **not on `main`** takes the number of the `main` commit it
branched from. A branch build therefore never outranks the `main` build that
follows it — a device that ran it still upgrades — and it is for devices
only: nothing uploads it to a store, because it shares its number with its
base.

**How to read a build number:** `git rev-list --count <commit>` equals it for
a `main` commit. The release job logs the source commit beside the number and
records both.

### Every build path gets its tree from one place

The controller, the Ansible plays and `scripts/ship-physical` read the number
from the manifests of the checkout they build. So none of them builds a
working checkout: `validation/mobile_build.py` hands each one the commit plus
a *stamp commit* that writes the number into both manifests (never pushed).

```
                      validation/mobile_build.py
        build_number ─ release_source (pinned repo) ─ release_checkout
                 │               │                          │
   make android-release   mobile-channels.yml        scripts/ship --apple/--android
   (-PplurxBuildNumber)   mobile-physical.yml        scripts/ship-physical
                          (role asks for it)         (stamped checkout)
```

| Path | Gets its tree by | Refuses build `1` at |
|---|---|---|
| `mobile-channels.yml` on a push to `main` (TestFlight, Play internal) | `release_source` | the controller's metadata read |
| `scripts/ship --apple` / `--android` → Ansible `mobile.yml` | `mobile_build checkout`, passed as `plurx_repo` (`--apple` requires `main`) | `mobile.yml`'s first task |
| Ansible `mobile-physical.yml` | the `mobile_release` role runs `mobile_build release-source` | the role's metadata read |
| `scripts/ship-physical` | `mobile_build checkout` | `mobile_build require-stamped`, before any build |
| `make android-release` / `android-publish` | the number, as a Gradle property | the recipe, and again before the APK is served |

The Ansible rows need the companion change in `pjunod/ansible` (branch
`claude/cinema-release-stamp`): `noirr_mobile_metadata` refuses a Cinema
checkout that still carries build `1`, `mobile.yml` reads it first, and the
role stamps. Without it `scripts/ship` still works (it passes a stamped
checkout), and `mobile-physical.yml` stops at its release guard with nothing
built.

### The high-water mark

The commit count only rises while `main` is never rewritten. The release job
therefore appends every publication to `cinema-published-builds.jsonl` in its
state directory — platform, number, source commit, stamp commit, time — and
refuses a number at or below the last one for that platform before it builds.
A failed upload records nothing and may be retried. Before the first record
the floor is the last hand-claimed build (Apple 301, Android 207). The stores
remain the authority; this is the local refusal that comes before a
twenty-minute archive instead of after it.

### A hotfix

A store build comes from `main`, so a hotfix is a pull request like any
other, sent next: label it `train:solo` ([MERGE-TRAIN.md](MERGE-TRAIN.md)),
and the push that lands it publishes it under the next number. To put the fix
on a device before it lands, `scripts/ship --android` (or `make
android-release`) from the branch builds it under its base's number; the
`main` build that follows replaces it.

Status lines name the version only, and `docs/STATUS.html` states no current
build (history stays in `<span data-build-history>`). What each Android build
through 207 changed is in
[clients/android/BUILD-HISTORY.md](../../clients/android/BUILD-HISTORY.md);
Apple per-change notes stay in [docs/apple-builds/](../apple-builds/README.md),
now without a `Build:` line.

## Reviewed counts (`tests/rolling-producer-owners.d/`)

The ledger pins one total per shape. A change records its own difference and
its ownership review:

```toml
[[changes]]
id = "namespaced-task-spawn"
delta = 1
reason = "One joined test-only releaser task in <test>; the test awaits it."
```

The reviewed count is the ledger's `expected_occurrences` plus every `delta`.
Two changes that each add a spawn sum to two in either merge order; with one
shared total they both wrote the same next number, merged cleanly, and left
`main` one short. The failing test prints the rows to add. A count still
cannot change without a recorded, reasoned row, and `scripts/paperwork`
refuses an edit to a total in the ledger itself.

## Client fix anchors (`tests/client-fixes.d/`)

One or more `[[fixes]]` tables per file, same keys as before. The ledger keeps
its existing rows, `version` and `enforce_after`; `load_client_fixes` reads
it, then the fragments in name order, and refuses an id used twice.
Format: [tests/client-fixes.d/README.md](../../tests/client-fixes.d/README.md).

## Governed paths (`validation/points.d/`)

Most edits to the catalog add a path to an existing point, at the same few
list tails. A change records its paths instead:

```toml
[[paths]]
point = "playback.pipeline"
add = ["scripts/vod-plan-probe"]
```

`load_catalog` appends them to the point, so `scripts/validate lint` and every
lane see them. A new point, check, timeout or setting still goes in
`validation/points.toml`. Format:
[validation/points.d/README.md](../../validation/points.d/README.md).

## Status (`status.d/`)

One effort per file: `# Title`, then owner, state and a link to the ledger; at
most 30 lines and 2,400 bytes. Edit your own file as the effort moves, delete
it when the effort ends. `scripts/status` prints them newest first
(`--full` for the text, `-n 10` for the newest ten). `STATUS.md` is a fixed
page. Format: [status.d/README.md](../../status.d/README.md).

## An open pull request, after this lands

```bash
git merge origin/main          # conflicts in the shared files are expected
scripts/migrate-bookkeeping    # or: make migrate-bookkeeping
git commit
```

The tool works out what the branch added to each shared file since it left
`main`, writes that as fragments named after the branch, takes the additions
back out, and three-way merges what is left with `main`'s copy — so each
shared file ends exactly as `main` has it and the merge has no bookkeeping
conflict left. It moves changelog lines, new client fix rows, count changes
(with the review comment as the `reason`), paths added to a point, and status
entries (an update to an effort `main` already moved lands on that effort's
file); it drops a claimed build number and the Android status narrative,
which it prints so you can keep it in your fragment. Everything is staged,
nothing committed.

**How to read it:** `moved` lines are done. A `MANUAL` line names a file whose
other edits genuinely conflict with `main`'s; resolve that one by hand.
Anything under "still conflicted" is your code, not bookkeeping.
`scripts/paperwork` names whatever is left — typically a build number quoted
in a status line or a `Build:` line in a new Apple fragment.

## What is still shared, and why

Touches in the 150 `main` commits before 2026-11-15; the six above were
CHANGELOG 53, owner counts 34, points.toml 31, client-fixes 22, STATUS.md 19
and the build number.

| File | Touches | Why it is not a per-change file yet |
|---|---|---|
| `verification/durable-jobs/mapping.json`, `verification/operation-progress/mapping.json` | 32, 31 | Not an append. Each row pins the SHA-256 of a whole source file, so any edit to that file must restate it, and two edits to one file merge to content neither hash describes. Per-entry files would hide the conflict and fail on `main` instead. Removing it means changing what "reviewed" covers (the anchored function rather than the file), which is the formal-model owners' decision. |
| `docs/features/PROJECT-BACKLOG.md` | 32 | One row per project at an anchor. The same per-file layout fits; it is a status ledger with its own readers and was left for a change of its own. |
| `docs/streaming/README.md` and the other folder indexes | 27 | An index row per document. `make paperwork-fix` already writes a missing row; removing the shared edit means generating the index, which was left for a change of its own. |
| `validation/native_receipts.py` | 22 | Reviewed hashes of build recipes (`Makefile`, `project.yml`). The same shape as `mapping.json`: a hash of a whole file. |
| `tests/operations/test_contracts.py` | 20 | Test code, edited at different places; conflicts there are real ones. |
| `docs/STATUS.html` | 16 | Hand-written HTML. Its build-number claims, the part every client change edited, are gone; the rest is prose edits at different places. |
| `docs/API.md` | 14 | One entry per route, inside the section for its area, so two changes meet only in the same section. `test_api_doc_routes` keeps it complete. |

Two changes to the same source line still conflict. That is a real conflict,
not bookkeeping.

Kept honest by `tests/operations/test_shared_bookkeeping.py`,
`test_migrate_bookkeeping.py`, `test_status_page_shape.py`,
`test_release_cut.py`, `test_mobile_channels.py`,
and `tests/validation/test_mobile_build.py`, `test_mobile_versions.py` and
`test_rolling_producer_ownership_inventory.py`.
