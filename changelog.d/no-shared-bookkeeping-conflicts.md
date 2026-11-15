### Changed

- TestFlight and Google Play builds are numbered when they are released, by
  the number of commits in the `main` commit they are built from; the first
  such build is above 15,000, after Apple 301 and Android 207. No pull
  request claims a build number, and a client change needs no bump.
- Changelog entries, client fix anchors, reviewed task/timer count changes and
  effort status entries are one file per change (`changelog.d/`,
  `tests/client-fixes.d/`, `tests/rolling-producer-owners.d/`, `status.d/`),
  so two pull requests no longer conflict on them
  ([docs/ci/SHARED-BOOKKEEPING.md](docs/ci/SHARED-BOOKKEEPING.md)).
