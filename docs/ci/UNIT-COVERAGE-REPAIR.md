# Unit coverage repair — restore the suite and cover missing behavior

**Status:** open · **Updated:** 2026-10-29

The 2026-10-28 workspace measurement covered 379,922 of 487,771 lines
(77.8894%) and completed with 24 failing tests. This effort repairs every
failure and targets at least 85% measured line coverage, with 90% as the
upper target. Keep the existing source scope; do not exclude difficult code
or mark tests ignored to increase the percentage. Hardware qualification
remains distinct from portable behavior tests.

Repair `7080f5fd` passes all 31 previously failing cases within 110 focused
regressions. Its isolated sharing preflight exposed two further failures with
one shared playlist-validation cause, described below. The replacement full
workspace and coverage campaign remain pending; focused success is not a
completed coverage measurement.

## Execution and ownership

This independent repair PR targets `main` through the ordinary affected-surface
validation described in `AGENTS.md` and `DEVELOPMENT_PIPELINE.md`. Require the
focused regressions, independent adversarial review, paperwork and a green
Main promotion gate on the current candidate. The table below records internal
file ownership. Rust work uses the existing isolated Linux coverage build slot,
pinned Rust 1.97.1, and source archives without Git metadata or credentials.
Preserve warm compiler artifacts and restore reserved runner resources at
completion.

| Repair area | Files owned | Acceptance |
|---|---|---|
| Repair stale core fixtures and contracts | `crates/plurx-core/tests/support/queue_fixture.rs`, `tests/store_contract.rs`, `tests/store_contract/subtitle_text.rs`, `src/store/sqlite/dv_conversion.rs`, `src/store/consistent_read_census.rs`, `src/store/hiqlite_fragment_index_cluster.rs` | Historical rewinds refuse retained data and partial schemas; Store methods have behavioral coverage; the authority ceiling stays unchanged. |
| Repair media and daemon failures | `crates/plurxd/src/availability.rs`, `fontenv_tests.rs`, `fragindex/tests.rs`, `hevc_census.rs`, `http/hls/tests/chunk_01.rs`, `http/tests.rs`, `live_tv.rs`, `live_tv/tests.rs`, `startup_tests.rs`, `vod/shared.rs`; `crates/plurx-core/src/transcode/mod.rs`, `index_pipe_tests.rs` | All original focused regressions pass; retaining HEVC preserves its admitted HLS sample entries and source PPS updates; incomplete decoder configuration is refused; valid bounded PUT staging cannot abort Live TV. |
| Repair timestamp-sensitive byte verification | `crates/plurxd/src/subtitle_models/artifacts.rs`, `ffmpeg.rs`, `ffmpeg/tests.rs` | Same-tick model/reporter rewrites cannot reuse old byte proof; settled executable reuse stays cheap. |
| Cover HTTP lifecycle behavior | `crates/plurxd/src/http/{dvr,library_channels,live_tv}.rs` and their new `_http_tests.rs` modules | Real SQLite-backed handlers prove lifecycle, input, ownership, pagination, and cleanup behavior. |
| Measure and close remaining gaps | Coverage workflow and narrowly scoped tests selected from uncovered-region reports | Applicable replicated contracts and isolated network fixtures run; final full workspace coverage reaches at least 85% with all applicable tests passing. |

## Original failures and causes

| Group | Tests | Cause |
|---|---:|---|
| Historical SQLite rewind fixtures | 6 | Fixture recognizes schema 115 while the measured source has schema 119; new caption and continuity schemas need strict empty-only removal. |
| Migration inventory | 1 | Downgrade inventory omits four migrations. |
| Consistent-read census | 4 | Two file counts and one new membership authority annotation were missing. |
| Store method inventory | 1 | Four text-translation/shared-caption methods lacked registered behavioral contracts. |
| Availability, HLS, readiness, split-child logging | 4 | Source wrappers/format and explicit diagnostic channels changed without updating the relevant assertions; one child log lacks its parent target. |
| Font selection | 1 | Bare rejection glob does not reject every absolute font path on the runner's Fontconfig. |
| Fragment-index budget | 1 | Fixture claims an unavailable Dolby bitstream filter, so the child fails before its time budget. |
| Retained HEVC | 1 | Fixture assumes retained PPS updates qualify an immutable `hvc1` init; the runner produces incomplete out-of-band decoder configuration, which production correctly refuses. |
| Live TV startup | 2 | Scanner rejects legitimate atomic PUT temporary filenames; scripted producer uploads before draining tuner input. |
| Held reporter/model identity | 3 | Immediate same-length writes can share filesystem timestamps, invalidating metadata-only digest reuse. |

## Exact failed regressions

### plurx-core library

- `store::background_jobs_copy_output::tests::encoded_output_upgrade_preserves_copy_publication_and_adds_exact_source_guards`
- `store::background_jobs_tests::background_subtitles_reconcile_historical_ready_demand_without_overwriting_history`
- `store::consistent_read_census::consistent_read_census_admits_a_raised_entry_with_an_authority_reason`
- `store::consistent_read_census::consistent_read_census_matches_the_checked_in_table`
- `store::consistent_read_census::consistent_read_census_refuses_a_new_authority_read_through_a_dispatch_helper`
- `store::consistent_read_census::consistent_read_census_refuses_a_synthetic_extra_read_in_a_fixture_copy`
- `store::sqlite::dv_conversion::tests::sqlite_v43_guard_migration_refuses_an_unrecoverable_committed_claim`
- `store::sqlite::dv_conversion::tests::the_downgrade_fixture_undoes_every_migration_after_the_guard`

### Store contracts

- `contract_inventory_matches_every_store_method`
- `sqlite_v69_migration_from_v68_preserves_file_grants_and_live_analysis_requests`
- `sqlite_v70_migration_adds_the_read_indexes_and_keeps_the_catalogue`
- `sqlite_fresh_and_upgrade_fragment_prune_plans_and_work_are_bounded`

### plurxd daemon

- `availability::tests::the_playback_start_path_never_reads_the_cache`
- `ffmpeg::tests::held_clock_reporter_bytes_enter_existing_engine_identity_and_publication_fence`
- `fontenv::tests::a_capture_whose_frozen_faces_diverge_is_refused`
- `fragindex::tests::a_shared_pass_that_runs_out_of_budget_fails_every_identity`
- `hevc_census::tests::a_retained_film_still_indexes_as_vod_presentable`
- `http::hls::tests::a_processed_candidate_that_cannot_be_adopted_or_consumed_degrades_with_its_reason`
- `http::tests::developer_readiness_reports_what_it_reads_and_admits_what_it_cannot`
- `live_tv::tests::a_cold_start_probes_before_it_spawns_and_caches_the_facts`
- `live_tv::tests::a_warm_start_that_disagrees_restarts_cold_and_never_publishes_the_wrong_plan`
- `startup_tests::split_children_log_under_their_parent_target`
- `subtitle_models::artifacts::local::tests::identity_cache_cannot_hide_model_replacement`
- `subtitle_models::artifacts::local::tests::metadata_identity_never_qualifies_changed_or_unsafe_model_bytes`

## Full-suite follow-up

The first combined schema-120 run exposed five additional failures. Two
assertions still expected version 119 after upgrade. The historical retarget
fixture also retained a managed-v2 marker and resource view while claiming
version 117; restoring the exact published v1 predecessor preserves populated
operations and receipts through migration 120. The executable digest cache test
now observes the existing two-second settling grace. The shutdown fixture uses
the production two-thread speech admission rather than a machine-dependent
whole-pool fragment admission. Its child-start, reap, private scratch and
admission-retention assertions remain intact. These five cases are required
preflight regressions for the replacement full measurement.

The subsequent full run found a discarded invitation-cancellation result and
an HTTP fixture relying on the old invitation default. Cancellation now uses
the closed Store-result observer. The fixture first asserts the current
network-authorization refusal, then explicitly selects the manually provisioned
sharing path; the isolated transport and restart fixtures select that same
path. All 31 known cases passed within 110 focused regressions on `7080f5fd`.

The next isolated sharing preflight passed nine daemon cases and the separate
restart case, but prepared handoff and quality reopen both returned 503 for the
successor playlist. Both request a nonzero start, whose actual producer emits
`EXT-X-START` with `PRECISE=NO`; the strict sharing validator allowed only
`TIME-OFFSET`. The correction accepts optional unquoted `PRECISE=YES|NO`, as
specified in [RFC 8216 section 4.3.5.2](https://www.rfc-editor.org/rfc/rfc8216.txt).
Unknown attributes, duplicate keys, quoted or invalid precision values,
URI-bearing additions and nonfinite or out-of-bounds offsets remain refused.
The existing producer regression passes actual attachment and acquisition
playlists through that validator. The two real sharing regressions must pass
before the next long default suite starts.

## Reproducing the measurement

The manual workflow uses `scripts/unit-coverage` on the existing capacity-one
`unit-memory-16g` host registration. It archives committed source without Git
metadata, creates one bounded standard Docker child, and uses an isolated CGNAT
bridge for sharing fixtures. No additional container capabilities or host
network namespace are required. The command preserves source archives, full
logs, exact Cargo artifact manifests and coverage receipts outside compiler
cache directories; cleanup targets only its labelled container and network.

The fixed corpus preflights sharing admission and Store-result auditing, then
runs the existing ignored sharing tests before the long full default workspace
suite and replicated feature graph. Both complete artifact
manifests remain in the report denominator. The augmented core replay uses the
reviewed cluster/Hiqlite prefix set plus every feature-added test only while its
reviewed source digest matches; any drift selects the full augmented core
suite. The eight affected integration targets run completely. Optional `--native`
adds twelve named software-media/storage tests, never hardware qualification.
No corpus phase stops early based on the measured percentage.

Clean-environment probe and desktop-control children deliberately discard the
profile output environment variable. Postflight preserves their raw profiles
only from the two source-backed package directories, requires LLVM binary IDs
from exact hashed objects of the same source archive, and moves them into the
fresh profile pool with a non-overwriting provenance journal. The thumbnail
cancellation fixture's exact `partial` pipe-output file is preserved outside the
checkout. Initial source verification remains strict; unknown outputs and
unarchived Rust source still fail postflight.

Only a passing required corpus creates `cov.json` and the success receipt used
for publication. Source/object mismatches, unexpected feature changes and failed
commands preserve diagnostic logs but cannot update the badge. Collector guard
and host-orchestration tests are separate from native execution evidence; the
final measurement receipt records which corpus actually ran. Successful badge
publication places a sanitized `coverage-receipt.json` beside the SVG and
endpoint JSON, recording the measured source/archive/report identities and
exact line counters in the same commit. Its visible date comes from the UTC
measurement timestamp.

## Additional behavior found by the new tests

The retained-HEVC regression preserves the existing `hvc1`/`dvh1` HLS and client
contract. Retaining PPS updates still protects decoded pixels; it does not
authorize an incomplete decoder configuration or a different client sample
entry. The native pixel-integrity proof remains separate from VOD admission.

DVR recording-list SQL reused a cursor placeholder for state JSON and supplied
five parameters to a four-parameter query. Correct bindings and a direct Store
regression now cover cursor ties, state/deleted history, capture bounds and
limits together. The Hiqlite DVR binder was already correct.

The new real membership test also found maintenance queries whose placeholder
order swapped positional Hiqlite bindings. The entry transaction omitted its
required release intent, heartbeat failed to acknowledge the current process,
and status hid valid acknowledgments while miscounting active sessions. Correct
binding order preserves the exact coordinator claim and atomic transition. A
real confirmed session now proves that acknowledgment alone cannot permit
maintenance exit; exit succeeds only after durable session End.

The full synthetic subtitle execution case also reproduced `engine_unavailable`
with the normal unbundled PATH defaults. Media tools now use the shared
canonical executable resolver before a clean launch, including self-test
windows; invalid explicit tools and cancellation still fail closed.

New coverage also exercises real SQLite-backed subtitle translation APIs,
synthetic recognition execution with actual media decoding, cluster singleton
admission/lease fences, held Dolby Vision observation contracts, catalogue
backfills, and durable media-release cleanup. Synthetic helper tests establish
protocol behavior, never installed-model or hardware qualification.

## Evidence

Baseline: coverage run 6087, source `77e9858277813456bb94e855ec62ed3e9fd65af8`.
The measurement is evidence of executed lines; its 24 failures do not establish
passing unit qualification. Fixes and new tests are under validation. Record
final source identity, focused results, full-suite result, measured covered/
total lines and gate receipts here before declaring this effort complete.
