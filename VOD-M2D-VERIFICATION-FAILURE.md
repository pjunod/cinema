# VOD M2D verification failure — handoff for Opus

**Status:** blocked before push · **Branch:** `agent/vod-m2d` · **Written:**
2026-08-24

This is a diagnostic handoff for the transport-only VOD M2D branch. Read §2
for the exact environment and §4 for the complete failure list. The branch
must not be pushed or rewritten until `cargo test --workspace` passes; do not
silently patch, amend, rebase, squash, or reword its commits.

## 1. Branch identity — the bundle gates passed

The branch was loaded from:

```text
target/vod-m2d-31b6853a.bundle
```

The required identity checks matched exactly:

| Check | Observed |
|---|---|
| Branch | `agent/vod-m2d` |
| HEAD | `31b6853aeafec783a65609d35d2e00cd2f402706` |
| `origin/main` | `951087043159eecfee0c8e1912215636ccac3507` |
| Commits in `origin/main..agent/vod-m2d` | `10` |
| Working-tree edits made during verification | None |
| Pushed | No |
| PR opened | No |

The two root-level untracked files that predated this work were left alone:

```text
VOD-M2-QUESTIONS-UPDATE-1.md
VOD-M2-QUESTIONS.md
```

## 2. Environment — Apple Silicon macOS

```text
macOS 26.6.2 (25G83)
arm64
rustc 1.95.0 (59807616e 2026-04-14) (Homebrew)
cargo 1.95.0 (f2d3ce0bd 2026-03-21) (Homebrew)
host: aarch64-apple-darwin
LLVM 22.1.3
TMPDIR=/var/folders/tf/pkdvxdp91gg7nmg809p2_f740000gn/T/
```

## 3. Verification — four gates passed, workspace tests failed

The required command sequence was:

```bash
make lint && \
  make fmt-check && \
  make history-check && \
  make validation-lint && \
  cargo test --workspace
```

These gates passed without changing the tree:

```text
make lint
make fmt-check
make history-check
make validation-lint
```

The history and validation outputs included:

```text
history ok: 784 corrective commits · 553 direct test changes · 413 explicit current-check mappings · 71 client-fix anchors · 30 non-runtime corrections
catalog ok: 22 points · 27 checks · 963 audited files
```

The first workspace run was inside a restricted execution sandbox. Twelve
`plurx-cluster-check` harness tests could not bind loopback ports and failed
with `Operation not permitted (os error 1)`. That was isolated as an execution
restriction: the unchanged suite was rerun with ordinary local socket access,
and all 26 harness tests passed.

The unrestricted run then failed in `plurx-core`:

```text
test result: FAILED. 653 passed; 28 failed; 0 ignored; 0 measured; 0 filtered out; finished in 32.11s
error: test failed, to rerun pass `-p plurx-core --lib`
```

## 4. Failures — 28 tests across three clusters

### 4.1 Secure filesystem capabilities

```text
fs_secure::tests::bounded_tree_removal_clears_a_nonempty_preflighted_tree
fs_secure::tests::bounded_tree_removal_preflights_before_unlinking_any_entry
fs_secure::tests::regular_child_placement_accepts_hardlink_ctime_change
```

Representative output:

```text
thread 'fs_secure::tests::bounded_tree_removal_clears_a_nonempty_preflighted_tree' panicked at crates/plurx-core/src/fs_secure.rs:1656:14:
bounded removal: Os { code: 20, kind: NotADirectory, message: "Not a directory" }

thread 'fs_secure::tests::bounded_tree_removal_preflights_before_unlinking_any_entry' panicked at crates/plurx-core/src/fs_secure.rs:1638:9:
assertion failed: error.to_string().contains("exceeded")

thread 'fs_secure::tests::regular_child_placement_accepts_hardlink_ctime_change' panicked at crates/plurx-core/src/fs_secure.rs:1674:14:
source capability: Os { code: 20, kind: NotADirectory, message: "Not a directory" }
```

### 4.2 Metadata publication

```text
metadata::book::tests::curator_identity_without_provider_origin_rebuilds_the_embedded_epub_cover
metadata::local::tests::home_artwork_adopts_generates_and_inherits
metadata::tests::a_known_tmdb_id_is_used_directly_and_the_search_is_never_called
metadata::tests::artwork_publication_stays_bound_to_the_open_parent_directory
metadata::tests::cancellation_during_blocking_publication_leaves_no_output
metadata::tests::enrich_library_matches_movie_show_and_episode
metadata::tests::enrich_library_touches_only_the_ids_it_was_given
```

Representative output:

```text
thread 'metadata::book::tests::curator_identity_without_provider_origin_rebuilds_the_embedded_epub_cover' panicked at crates/plurx-core/src/metadata/book.rs:1512:9:
a stale caller must leave the published final generation for the fenced grace sweep

thread 'metadata::tests::artwork_publication_stays_bound_to_the_open_parent_directory' panicked at crates/plurx-core/src/metadata/mod.rs:1382:10:
publish through held directory capability: Os { code: 20, kind: NotADirectory, message: "Not a directory" }

thread 'metadata::tests::cancellation_during_blocking_publication_leaves_no_output' panicked at crates/plurx-core/src/metadata/mod.rs:1330:14:
publication started: RecvError

thread 'metadata::tests::a_known_tmdb_id_is_used_directly_and_the_search_is_never_called' panicked at crates/plurx-core/src/metadata/mod.rs:1994:9:
assertion failed: m.poster_path.is_some()

thread 'metadata::local::tests::home_artwork_adopts_generates_and_inherits' panicked at crates/plurx-core/src/metadata/local.rs:589:9:
assertion `left == right` failed: report: LocalArtReport { adopted: 0, generated: 0, inherited: 0, errors: 2 }
  left: 2
 right: 0
```

### 4.3 Transcode manifest capability handling

```text
transcode::manifest::tests::blank_checkpoint_tail_is_rejected_in_constant_record_work
transcode::manifest::tests::budgeted_manifest_load_refuses_before_reading_past_its_allowance
transcode::manifest::tests::cached_checkpoint_cursor_is_bound_to_the_requested_object_prefix
transcode::manifest::tests::capability_checkpoint_validation_resumes_across_small_preemption_budgets
transcode::manifest::tests::capability_publication_checkpoints_sub_interval_progress_before_yielding
transcode::manifest::tests::checkpoint_appends_every_permitted_record_across_retries
transcode::manifest::tests::checkpoint_growth_after_open_is_rejected_without_a_large_line_allocation
transcode::manifest::tests::generation_id_rejects_json_expansion_outside_the_header_contract
transcode::manifest::tests::mutation_between_cached_validation_and_publication_forces_rehash
transcode::manifest::tests::parseable_checkpoint_byte_count_corruption_is_rehashed
transcode::manifest::tests::parseable_checkpoint_digest_corruption_is_rehashed
transcode::manifest::tests::playlist_authenticates_the_response_buffer_not_a_reopened_path
transcode::manifest::tests::publication_rejects_an_object_larger_than_the_scrub_io_ceiling
transcode::manifest::tests::requested_object_verification_detects_corruption_without_a_manifest_walk
transcode::manifest::tests::segment_streams_the_same_handle_that_was_authenticated
transcode::manifest::tests::stalled_snapshot_bodies_reject_same_class_admission_promptly
transcode::manifest::tests::validated_checkpoint_cursor_survives_a_later_object_hash_yield
transcode::manifest::tests::zero_length_object_yields_before_it_is_checkpointed
```

Representative output:

```text
thread 'transcode::manifest::tests::blank_checkpoint_tail_is_rejected_in_constant_record_work' panicked at crates/plurx-core/src/transcode/manifest.rs:1975:14:
directory capability: Os { code: 20, kind: NotADirectory, message: "Not a directory" }

thread 'transcode::manifest::tests::budgeted_manifest_load_refuses_before_reading_past_its_allowance' panicked at crates/plurx-core/src/transcode/manifest.rs:1331:14:
publish manifest: "opening generation directory: Not a directory (os error 20)"

thread 'transcode::manifest::tests::publication_rejects_an_object_larger_than_the_scrub_io_ceiling' panicked at crates/plurx-core/src/transcode/manifest.rs:1444:9:
assertion failed: publish(directory.path(), "generation-too-large", &["seg00000.ts".to_owned()]).await.expect_err("oversized generation object").contains("exceeds")

thread 'transcode::manifest::tests::stalled_snapshot_bodies_reject_same_class_admission_promptly' panicked at crates/plurx-core/src/transcode/manifest.rs:2125:9:
assertion failed: matches!(observed, Err(VerifiedObjectError::Capacity))
```

## 5. Working hypothesis — shared directory-capability behavior on macOS

This is an inference, not a diagnosis. The failures cluster around held or
reopened directory capabilities, and the most common first error is:

```text
Not a directory (os error 20)
```

The metadata assertion failures may be downstream effects of publication
operations failing through the same capability path. The next useful step is
to run a single representative test with a full backtrace before changing
code:

```bash
RUST_BACKTRACE=full cargo test -p plurx-core \
  fs_secure::tests::bounded_tree_removal_clears_a_nonempty_preflighted_tree \
  -- --exact --nocapture
```

Then compare the directory-capability/open-at behavior on this
`aarch64-apple-darwin` host with the authoring environment. Do not assume the
28 assertions are independent defects until that shared failure is ruled out.

## 6. Acceptance — what must be true before transport resumes

Resume the push/PR task only when all of the following hold:

- `cargo test --workspace` exits zero on the unchanged branch, because the
  transport handoff requires the full workspace gate.
- `git rev-parse HEAD` is still
  `31b6853aeafec783a65609d35d2e00cd2f402706`, because the supplied history
  must not be rewritten.
- `git diff --check` is clean and no source files were edited, because this
  task is transport-only.

