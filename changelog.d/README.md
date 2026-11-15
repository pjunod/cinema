# Changelog fragments

One file per change. A pull request never edits `CHANGELOG.md`: it adds
`changelog.d/<slug>.md`, and `scripts/release-cut` folds every fragment into
the dated release section and deletes them. Two pull requests write two
files, so their changelog entries cannot conflict.

```markdown
### Fixed

- A leader restart no longer ends Live TV playback.
```

- Name the file after the change (`live-tv-leader-restart.md`): lower-case
  letters, digits, `.`, `-`, `_`.
- Headings are `### Added`, `Changed`, `Deprecated`, `Removed`, `Fixed` or
  `Security`; a fragment may carry several.
- `python3 -m validation.changelog_fragments --preview` prints the section
  the next release would get.
- `scripts/paperwork` rejects a malformed fragment and any line a branch adds
  under `## [Unreleased]`.

[docs/ci/SHARED-BOOKKEEPING.md](../docs/ci/SHARED-BOOKKEEPING.md) covers
every per-change directory.
