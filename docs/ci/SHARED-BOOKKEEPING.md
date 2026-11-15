# Shared bookkeeping — one file per change, so pull requests never meet

**Status:** live. Owners: the validators named per section. Added 2026-11-15.

Companion to [PAPERWORK.md](PAPERWORK.md) (which rules are checked, and when)
— this is *where a change records itself*, and what an open pull request does
after this layout landed.

Five files used to be edited by nearly every pull request at the same line,
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

```
 push to main ─▶ mobile-channels.yml ─▶ which platforms changed?
                                              │
                    build number = commits in that main commit
                                              │
        pinned repo: main = that commit + one stamp commit that
        writes the number into both manifests (never pushed)
                                              │
                     signing controller builds and uploads it
```

The controller reads the number from the manifests of the commit it is given
and refuses a platform whose sources changed after its number last did. The
stamp commit satisfies both without a controller change; a test runs the
controller's two checks against it
(`tests/validation/test_mobile_build.py`). `scripts/ship-physical` stamps its
disposable worktree the same way, and `make android-release` passes
`-PplurxBuildNumber`.

**How to read a build number:** `git rev-list --count <commit>` equals it.
The release job logs the source commit beside the number.

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
cannot change without a recorded, reasoned row.

## Client fix anchors (`tests/client-fixes.d/`)

One or more `[[fixes]]` tables per file, same keys as before. The ledger keeps
its existing rows, `version` and `enforce_after`; `load_client_fixes` reads
it, then the fragments in name order, and refuses an id used twice.
Format: [tests/client-fixes.d/README.md](../../tests/client-fixes.d/README.md).

## Status (`status.d/`)

One effort per file: `# Title`, then owner, state and a link to the ledger; at
most 30 lines and 2,400 bytes. Edit your own file as the effort moves, delete
it when the effort ends. `scripts/status` prints them newest first
(`--full` for the text, `-n 10` for the newest ten). `STATUS.md` is a fixed
page. Format: [status.d/README.md](../../status.d/README.md).

## An open pull request, after this lands

Merge `main` in, then:

1. **`CHANGELOG.md`:** move the lines you added under `[Unreleased]` into
   `changelog.d/<slug>.md` and restore `main`'s `CHANGELOG.md`.
2. **Build numbers:** take `main`'s side of any conflict in
   `build.gradle.kts`, `project.yml`, the client READMEs,
   `APPLE-CLIENT-PARITY.md` and `STATUS.html`; `make paperwork-fix` resets a
   number that merged without conflict. An Android narrative paragraph
   becomes your changelog fragment. Drop `Build:` from a new Apple fragment.
3. **Counts:** restore `main`'s `rolling-producer-owners.toml`; run
   `python3 -m unittest tests.validation.test_rolling_producer_ownership_inventory`
   and add the rows it prints, with your review as `reason`.
4. **Client fixes:** move the rows you added into
   `tests/client-fixes.d/<slug>.toml` and restore `main`'s ledger. An edit to
   an existing row's anchor stays where it is.
5. **`STATUS.md`:** move your entry into `status.d/<date>-<slug>.md` (links
   become `../docs/...`) and restore `main`'s `STATUS.md`.

`scripts/paperwork` names whichever of these is left.

## What this does not cover

- **Doc index rows, `validation/points.toml` and the project backlog** are
  still shared files. They did not conflict often enough this week to be in
  scope; the same layout would apply.
- **Two changes to the same source line** still conflict. That is a real
  conflict, not bookkeeping.

Kept honest by `tests/operations/test_shared_bookkeeping.py`,
`test_status_page_shape.py`, `test_release_cut.py`, `test_mobile_channels.py`,
and `tests/validation/test_mobile_build.py`, `test_mobile_versions.py` and
`test_rolling_producer_ownership_inventory.py`.
