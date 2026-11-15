# Reviewed count changes, one file per change

`tests/playback/rolling-producer-owners.toml` pins how often each task, timer,
process and legacy-owner shape occurs. A change that adds or removes one does
not edit that total: it records its own difference here.

```toml
# tests/rolling-producer-owners.d/pr1045-preparation-releaser.toml
[[changes]]
id = "namespaced-task-spawn"
delta = 1
reason = "One joined test-only releaser task in processed_preparation_waits_only_for_background_work_that_can_yield; the test awaits it."
```

The reviewed count is the ledger's `expected_occurrences` plus every `delta`
here, so two changes add up on `main` in either merge order. The failing test
prints the exact rows to add. `reason` is the ownership review the edit to the
total used to carry as a comment, and is required. A file holds one row per
shape; name it after the change.
[docs/ci/SHARED-BOOKKEEPING.md](../../docs/ci/SHARED-BOOKKEEPING.md) covers
every per-change directory.
