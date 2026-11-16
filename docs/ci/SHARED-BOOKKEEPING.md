# Shared bookkeeping — one file per change, so pull requests never meet

**Status:** live. Owners: the validators named per section. Added 2026-11-15.

Companion to [PAPERWORK.md](PAPERWORK.md) (which rules are checked, and when)
— this is *where a change records itself*, and what an open pull request does
after this layout landed.

Seven files used to be edited by nearly every pull request at the same line,
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
| A patch to vendored hiqlite / hiqlite-wal | `vendor/<crate>/patches.d/<name>.toml` | a numbered row or the count in `PLURX-PATCH.md` |
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

The commit count only rises while `main` is never rewritten, so every store
upload records itself **in the forge**: `refs/cinema-published/<platform>` on
`origin` is moved to the `main` commit that was published
(`ios` · `tvos` · `android`). Before any build, the release job and
`scripts/ship --apple` fetch those refs and refuse a number at or below the
one they hold. The floor therefore survives a new controller or a wiped state
directory, and both upload paths share it.

A ref only moves forward. The push is leased on the value just read
(`--force-with-lease`), so a ref another publisher moved meanwhile is read
again, never overwritten, and a commit with a lower count never replaces a
higher one. On one host the two upload paths cannot overlap at all: the
release job and `scripts/ship --apple` hold the same `publish.lock` in the
controller's state directory, from the floor check to the record.

**How to read it:** `git ls-remote origin 'refs/cinema-published/*'` shows the
last published commit per platform; its build number is
`git rev-list --count <commit>`. Before the first ref exists the floor is the
last hand-claimed build (Apple 301, Android 207). A failed upload records
nothing and may be retried. The stores remain the authority.

After an upload the controller first appends platform, number, source commit,
stamp commit and time to `cinema-published-builds.jsonl` in its state
directory, then pushes the ref. That file is a second, local check, not the
floor — and it is what covers a failed push:

| The run ended | The ledger file | The forge ref | The next run on that host |
|---|---|---|---|
| upload failed | no line | not moved | builds and uploads the same number again |
| uploaded, ref pushed | line | moved | refuses that number; publishes the next commit |
| uploaded, **ref push failed** (run is red) | line | not moved | pushes the ref and uploads nothing for that commit; a later commit is published only once the push has gone through |

`python3 -m validation.mobile_build retry-unrecorded` does that retry on its
own and prints each ref it moved.

A stamp commit ends with `Build-Scope: store` or `Build-Scope: devices`. Only
a commit on `main` gets `store`, and the Ansible upload task refuses anything
else, so a branch checkout cannot reach TestFlight by hand.

**Where the channel playbook lives:** `media/mobile-channels.yml` is not
tracked in `pjunod/ansible` (checked on every branch, 2026-11-15); it exists
only on the signing controller. Whether it reads the build number through
`noirr_mobile_metadata` cannot be verified from either repository, so the
first publish after this lands is its acceptance.

### Landing order

This change and its Ansible companion (`pjunod/ansible` #18) land in this
order. Two Ansible copies matter, and they move at different steps:
`~/code/ansible/media` runs `mobile-physical.yml` and `scripts/ship`; the
channel job runs a pinned snapshot under
`~/.local/share/plurx-mobile-publisher/versions/`, named by
`PLURX_ANSIBLE_DIR`.

1. **The #18 fix.** `read_plurx` applies the stamped rules only to a tree
   that has `validation/mobile_build.py`; a tree from before this change is
   read as it always was (Apple 301 / Android 207 differ, and pass). Without
   it the new role refuses today's `main`.
2. **Merge #18 and update `~/code/ansible/media`.** From here no path can
   build development build `1`: the old role would have built `origin/main`
   unstamped, and `devicectl` accepts a lower build number than the device
   holds.
3. **Land this change (#1648) with `train:solo`.**
4. **Install a new publisher snapshot from the #18 merge and repoint
   `PLURX_ANSIBLE_DIR`.** Never move the pinned snapshot to the #18 head
   before #1648 is on `main`. The old snapshot is safe meanwhile: it is
   handed the stamped pin.
5. **The first client push is the acceptance test** — of the untracked
   `mobile-channels.yml` and of the push to `refs/cinema-published/*`, which
   the controller's credential has not made before.

**How to read where it stands:** `git -C ~/code/ansible/media log -1` shows
step 2; `validation/mobile_build.py` on `origin/main` shows step 3;
`echo $PLURX_ANSIBLE_DIR` on the signing runner shows step 4;
`git ls-remote origin 'refs/cinema-published/*'` shows step 5.

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

`scripts/release-cut` is the ledger's one writer: it folds every recorded
delta into its total, keeps each reason as a comment on the row, and deletes
the files, so the directory starts each release empty. The fold is worked out
before the cut writes anything, so a file it cannot fold — an unknown shape, a
total taken below zero — refuses the cut with the tree untouched. Paperwork tells that
fold from a hand edit — a total may move only by the deltas of the files
deleted with it.

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
`validation/points.toml`, and so does a rename or a narrowed glob of an
existing path: paperwork refuses only a point's list growing. Format:
[validation/points.d/README.md](../../validation/points.d/README.md).

## Vendored patches (`vendor/hiqlite*/patches.d/`)

`PLURX-PATCH.md` numbered each patch and stated the total in words, and the
ledger test repeated both. Every pull request that added a patch took "the
next number" — four were open at once on 2026-11-15, all claiming 29. The
numbered rows are closed; a later patch is a file keyed by its name, with the
row's fields (`patch`, `kind`, `upstream`, `drop`) and its bullet as
`description`. The same rules apply, a file named in a description counts as
owned for `PLURX-FILES.toml`, and nothing states a total:
`python3 -m validation.vendor_patches` derives it (rows plus files). Format:
[vendor/hiqlite/patches.d/README.md](../../vendor/hiqlite/patches.d/README.md).

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
(with the review comment as the `reason`), paths added to a point, vendored
patch rows with their bullets, and status
entries (an update to an effort `main` already moved lands on that effort's
file); it drops a claimed build number and the Android status narrative,
which it prints so you can keep it in your fragment. Everything is staged,
nothing committed.

**How to read it:** `moved` lines are done. A `MANUAL` line names a file whose
other edits genuinely conflict with `main`'s; for that file nothing was
written — no fragment, no change — the line says exactly what to do, and the
run exits non-zero. Conflict markers a branch committed are refused, never
carried into a fragment.
Anything under "still conflicted" is your code, not bookkeeping.
`scripts/paperwork` names whatever is left — typically a build number quoted
in a status line or a `Build:` line in a new Apple fragment.

## What is still shared, and why

Touches in the 150 `main` commits before 2026-11-15; the six above were
CHANGELOG 53, owner counts 34, points.toml 31, client-fixes 22, STATUS.md 19,
the build number and the vendored patch ledgers.

| File | Touches | Why it is not a per-change file yet |
|---|---|---|
| `verification/durable-jobs/mapping.json`, `verification/operation-progress/mapping.json` | 32, 31 | Not an append. Each row pins the SHA-256 of a whole source file, so any edit to that file must restate it; 23 of the 32 touches changed only `sha256` values. Pinning the anchored item instead of the file would remove those 23. It is not done here because the packages say the whole-file pin is deliberate ("Whole-file source hashes are intentional", `verification/operation-progress/README.md`; "Pins are refreshed by whole file", `durable-jobs/CONTRACT.md`): narrowing it changes what a review covers, which is the owner's ruling to make. |
| `validation/main-preflight-inputs.json` | 5 | Hand-kept input closures per test family; this change alone edited about thirty to add one imported module. They are derivable from each module's imports, but the digest of those inputs is what every stored unit receipt is matched against, so deriving it invalidates all of them once. Left for a change that schedules that rerun. |
| `validation/merge-errata.toml` | 12 | Append-only rows at the end of one file. The per-file layout fits (as `validation/regressions.d/` already does for its ledger); not converted here. |
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
`test_release_cut.py`, `test_mobile_channels.py`, `test_hiqlite_patch_ledger.py`,
and `tests/validation/test_mobile_build.py`, `test_mobile_versions.py` and
`test_rolling_producer_ownership_inventory.py`.
