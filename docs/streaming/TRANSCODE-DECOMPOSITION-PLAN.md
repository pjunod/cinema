# Transcode decomposition — behaviour-preserving moves first, redesign later

**Status:** ready for review · **Executes:** §4.1, §4.2, §4.9, F-stream-15,
F-core-10, F-sc-12, F-hist-13, F-build-ops-codehealth-3 and -14 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
(§5.3 "this quarter", size L) · **Written:** 2026-09-20 against `main` @
`88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Companion to [WEB-SHELL-LAYOUT.md](../clients/WEB-SHELL-LAYOUT.md) and
[WEB-SHELL-SPLIT-PLAN.md](../clients/WEB-SHELL-SPLIT-PLAN.md) (the one split
this repo has done with a byte-identity proof, PR #371; this plan copies its
method), to [VOD-ENCODING.md](VOD-ENCODING.md) and
[PLAYBACK.md](../PLAYBACK.md) (what the code being moved does), and to the
review's §4.9 ownership map (the target boundaries). Read §2 for the shape
of the three files as they are and §3.1 for the three rules every move
obeys; then work §5 in order — each milestone is a separate draft PR, and
the order is chosen so that unrelated fixes can land on `main` between any
two of them. If a step appears to require changing what a function *does*
— a lock order, an `.await` point, a rejection order, an error string — to
make the move compile, stop and flag it: that is a redesign, and this plan
keeps redesign in its own milestones (§5.6–§5.8) with their own tests.

**Corrections to the review** (found re-verifying the appendix's line ranges
against `88a3957a`; the direction stands, the map needed these):

- `apply_ahead_window` is a `TranscodeManager` method at
  [`transcode.rs:25628`](../../crates/plurxd/src/transcode.rs), not in the
  1331–1755 flow block; the block holds `Ahead`, `AheadLimits`,
  `AheadHoldReason`, `AheadHold`, `FlowEvaluation`, `FlowInputs`,
  `SuspendedAt`, `RollingScratchReservation`.
- `SessionRequest` is at `:10183`, `Presentation` is `pub enum Presentation`
  at `:10246`, `SessionKind` at `:10267`, `DeliveryCandidate` at `:10143`;
  `StartInfo` (`:9540`) opens that region. There is no `HlsSessionInfo`
  type; `SessionInfo` is at `:9745`.
- `CachedLocationIdentity`/`CacheOfferVerdict` are at `:5168-5177` and
  `PreparedSharedCacheRead` at `:5844`, inside the attempt-child region,
  not beside `verify_cache_offer_location*` (`:15120`, `:15163`).
- The appendix proposes a `delivery.rs` inside the transcode tree;
  `crates/plurxd/src/delivery.rs` already exists (activity: who is watching
  what). The new module is named `response.rs` here.
- There is a second inline test module, `pretranscode_renewal_tests`
  (`transcode.rs:11357-11403`), in the middle of the file, and the main
  one is `pub(crate) mod tests` (`:27574`) because `http/hls.rs` tests call
  `crate::transcode::tests::activate_control_route` (`hls.rs:17237` and
  three more). Moving tests out must keep that path.
- `renditiondir.rs:162` (cited under L8 as a third HLS playlist parser) is
  `parse_segment_name`, a segment-file-name parser, not a playlist parser.
  The two playlist parsers are `transcode.rs:1107 parse_playlist` and
  `live_tv.rs:6487 parse_playlist_bytes`. Parser unification is L8's, not
  this plan's, and the assessment's rule applies to both: inventory each
  parser's accepted grammar before sharing one.
- `#[cfg(test)]` counts at `88a3957a`: 861 in `crates/*/src`, of which
  `transcode.rs` 210, `playback_control.rs` 149, `http/hls.rs` 60,
  `vodserve.rs` 49 (review: 208/148/60/49). The classified census is §2.6.

## 1. Objective

Turn `transcode.rs` (47,096 lines), `http/hls.rs` (28,507) and
`vodserve.rs` (15,011) into directories of modules whose boundaries follow
the §4.9 ownership map — contract → admission → producer attempt →
published object → client attachment — **without changing any behaviour**,
proven by a reassembly identity check per PR, and then, in separate
milestones with separate tests, (a) route the three ffmpeg spawn paths
through one helper, (b) make `ControlState::accept_at` a pure step, (c)
extract a `NodeLifecycle` transition function from `membership.rs`, and
(d) migrate the lifecycle test seams compiled into shipping structs to
injected hooks. Registry unification (the two session registries) is
*evaluated* at the end, not planned here, with its risks written down
(§3.6).

Why moves first: the assessment (F-stream-15, F-build-14, 4.1) accepted the
decomposition and rejected bundling it with redesign — "isolate moves from
semantic redesign", "extract first, preserve private visibility and
lifetimes, then separately evaluate unification". The web-shell split
showed a byte-identical move with a one-shot gate is reviewable in a day;
a move that also fixes something is not.

## 2. Contract today

Re-verify each citation at build time.

### 2.1 Sizes and shapes

| File | Lines | Product lines (before its last `#[cfg(test)]`) | Test module |
|---|---:|---:|---|
| `crates/plurxd/src/transcode.rs` | 47,096 | 27,573 | `pub(crate) mod tests` `:27574`; `mod pretranscode_renewal_tests` `:11357-11403` |
| `crates/plurxd/src/http/hls.rs` | 28,507 | 14,193 | `mod tests` `:14194` |
| `crates/plurxd/src/vodserve.rs` | 15,011 | 8,091 | `mod tests` `:8092`, which `include!("vodencode_tests.rs")` at `:8473` |
| `crates/plurxd/src/playback_control.rs` | 29,986 | 14,533 | `mod tests` `:14534` |
| `crates/plurx-core/src/cluster/membership.rs` | 17,169 | 10,708 | `mod tests` `:10709` |

`impl TranscodeManager` spans `transcode.rs:12942-26776`; the struct is at
`:12565` and its registry field is
`sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>` (`:12687`; the same
field type appears on four helper structs at `:3387`, `:3659`, `:3869`,
`:4044`). `VodServe` (`vodserve.rs:2136`) holds `Arc<Shared>` whose
registry is `sessions: Mutex<HashMap<String, Session>>` (`:2091`) — a
different `Session` type — plus `preparing_sessions` (`:2094`).

### 2.2 One request crosses three files and two registries

A segment GET: `hls::segment` ([`hls.rs:12683`](../../crates/plurxd/src/http/hls.rs))
→ `hls::authorize_response_publication` (`:9589`) →
`TranscodeManager::authorize_response_publication`
([`transcode.rs:23389`](../../crates/plurxd/src/transcode.rs)) and
`commit_authorized_media` (`:23957`) → for a VOD id, `VodServe` methods.
Both registries carry the same four operations under the same names:

| Method | `transcode.rs` | `vodserve.rs` |
|---|---|---|
| `promote_prepared_session` | `:22757` | `:2155` |
| `record_desired_persisted` | `:22772` | `:3828` |
| `response_owner_is_live` | `:24248` | `:3383` |
| `segment_window` | `:24961` | `:4676` |

The assessment's reading is the one this plan adopts: the two registries
"intentionally represent VOD versus retained rolling playback; unifying
them is not a mechanical move".

### 2.3 Three spawn paths, two progress parsers

- `spawn_ffmpeg` (`transcode.rs:2150`) and `spawn_ffmpeg_pipe` (`:2289`)
  apply `configure_ffmpeg_runtime` (`:2373`, `pub(crate)`) — the fontconfig
  `XDG_CACHE_HOME` fix and the Windows job object.
- `vodserve::spawn_generation` (`:6735`) builds
  `tokio::process::Command::new(recipe_program(recipe))` at `:6780` with
  `attach_recipe_descriptors`, `kill_on_drop(true)`, piped stdout/stderr —
  and does **not** call `configure_ffmpeg_runtime` or the Windows job
  helper. `regenerate_init_head` does the same at `:7876`.
- `is_progress_line`: the strict one at `transcode.rs:2392` accepts any
  `lower_snake_key=value`; the loose one at
  [`stream.rs:3409`](../../crates/plurxd/src/http/stream.rs) accepts only
  a fixed key list, so a bare `filter_units=…` diagnostic reads as
  progress there. (The assessment: swallowing that one does not prove
  swallowing every prefixed diagnostic; treat the grammar difference as
  the finding.)

### 2.4 `ControlState::accept_at`

[`playback_control.rs:3381-3620`](../../crates/plurxd/src/playback_control.rs):

```rust
    fn accept_at(
        &mut self,
        now: Instant,
        generation: &str,
        owner_epoch: u64,
        client_instance_id: &str,
        sequence: u64,
        acceptance: ControlAcceptance,
    ) -> Result<(ControlDisposition, u64, ControlAction, ClientPlatform, bool), ControlStateError>
```

`ControlState` (`:3125`) has 17 fields; `ControlStateError` (`:2197`) has
eleven variants; the function has 14 `return Err(...)` sites between
`:3381` and `:3620`, in this order: generation → owner epoch →
`sequence == 1` → platform → client instance → sequence monotonicity →
replay fingerprint → vocabulary → rate limit (`MIN_CONTROL_INTERVAL =
250 ms`, `:72`) → rollover → terminal directive → prepared-successor
binding. `ControlAcceptance` (`:3246`) carries `desired_digest` because
"observe runs after accept in both engines" (its doc comment). Only the
last sequence is retained: a resend of `N−1` after `N` and a replay of `N`
with a different body are both `StaleSequence` (`:3418`, `:3475`,
`:3491`, `:3497`). **These orderings and equivalences are the contract**
(assessment F-core-10: "rejection ordering, duplicate-sequence semantics
and effects are contractual"; changing the corrupted-replay answer "is an
API decision").

### 2.5 `membership.rs`

58 `*_SQL` constants; node state is inferred from rows plus capability
strings via `capability_ready_predicate` (`:2065`),
`node_is_tombstoned` (`:8948`), `local_node_is_committed_voter` (`:5073`);
procedures `heartbeat` (`:4272`), `enter_maintenance` (`:5527`),
`promote_learner` (`:7318`), `remove_voter` (`:7857`),
`reconcile_local_maintenance_commit` (`:5058`). The schema comment at
`:769-770` names the interval the promotion audit row exists for: "between
the blocking apply barrier and Hiqlite's joint/uniform voter-set
transition". Ambiguous write handling is a family of functions
(`cluster_operation_write_may_be_ambiguous` `:2566` and its callers at
`:5248`, `:5359`, `:5413`, `:5468`, `:5517`). The assessment (F-sc-12):
"joining, role, maintenance, removal attempts and tombstones have
overlapping dimensions; the illustrative flat enum is not a proven
exhaustive product state. Preserve joint-consensus reconciliation,
ambiguity and replicated exclusion before adopting a simplified transition
table."

### 2.6 Test seams: the classified census

The review's 681 (= 861 − 180 module attributes) was not a count of
shipping-struct mutations. This census is by what the attribute gates,
using the script in §3.7 (line-oriented; a `syn` pass confirms the
lifecycle class before a site is touched):

| Class | Count | Meaning |
|---|---:|---|
| `module` | 180 | `mod tests` and friends — not seams |
| `fn` | 250 | test-only helpers and fault-injection functions (`hls.rs` `injected_release_errors`, `release_pauses`, `fail_next_staged_read` …) |
| `field` | 169 | a struct field, enum arm or field initialiser present only under test |
| `statement` | 153 | a statement or block inside a shipping function present only under test |
| `type` | 31 | test-only types (`LifecycleTestPause`, `TerminalRouteTestOutcome`) |
| `impl` | 13 | test-only impls |
| `use` | 8 | imports |
| other | 35 | unclassified by the line rule (multi-attribute stacks) |

Plus 15 `#[cfg(not(test))]` twins (alternate production implementations:
`decode_facts.rs` ×4, `playback_control.rs` ×3, `hls.rs` ×2, one each in
`transcode.rs`, `subtitles.rs`, `dv_disk.rs`, `plurx-core transcode/mod.rs`,
`placeholder_census.rs`).

The lifecycle seams are the `field` + `statement` rows inside shipping
items (322 before de-noising; the line rule mis-files some `match` arms as
fields). Named examples, all real:

- `transcode.rs:3290-3293` `RollingRetirementSettlement`:
  `#[cfg(test)] wait_before_await_pause: std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>`,
  awaited twice at `:3357-3365` before `settled.await`.
- `transcode.rs:5298-5305` `AttemptChild`: `signal_after_authorization_pause`,
  `signal_after_flow_reservation_pause`,
  `terminate_before_reap_pause: Arc<std::sync::Mutex<Option<Arc<LifecycleTestPause>>>>`.
- `vodencode.rs:39` `Encoding`: `#[cfg(test)] admission_pause: Mutex<Option<Arc<tokio::sync::Barrier>>>`.
- `decode_facts.rs:258-274` `DecodeFactSource`: `initial_identity_delay`,
  `final_identity_delay`, with `#[cfg(test)]`/`#[cfg(not(test))]` getter
  twins at `:297-313`.

Why they matter, as the review states it: struct layout, lock acquisitions
and `.await` cancellation points differ between the test and release
binaries at those sites, so the supervisor orderings the tests pin are
proven for a test-shaped state machine. The existing precedents for the
replacement are the `scratch-fault-injection` cargo feature
([`plurx-core/Cargo.toml:34`](../../crates/plurx-core/Cargo.toml),
enabled only as a dev-dependency feature by
[`plurxd/Cargo.toml:79`](../../crates/plurxd/Cargo.toml)) and the
`PLURX_CLUSTER_ACTIVATION_FAILPOINT` env failpoint
([`migration.rs:138`](../../crates/plurx-core/src/cluster/migration.rs)).

### 2.7 The precedent: the web-shell split

WEB-SHELL-LAYOUT.md §1.3: the split commit carried a one-shot
`scripts/web-shell-identity` that reassembled the tree from the shell's
own tag order and compared it to the pre-split file, printed `OK against
d651fc61`, and was removed in the same PR ("a script in `scripts/` that
fails the first time anybody edits a web file is not a gate, it is a
trap"). The standing gates were the ones that already existed. One
relocation was needed and was named as the only one. This plan does the
same three things: an identity script per move PR, removed in that PR; the
existing test suite as the standing gate; every deliberate non-identity
named in the PR body.

## 3. Change

### 3.1 The three rules of a move PR

1. **Reassembly identity.** The PR carries
   `scripts/transcode-split-identity` (one-shot): it reads the module
   files in the order listed in the PR body, strips exactly the lines the
   split *added* (the `mod x;` declarations in the parent, each child's
   `use super::*;`-style header block, which the script recognises by a
   `// split: header` / `// split: end header` fence the PR adds), and
   diffs the concatenation against `git show <base>:crates/plurxd/src/transcode.rs`.
   Output is the web-shell format: `OK against <sha>` with per-file line
   counts, or the first differing hunk. `rustfmt --check` is run on the
   result *before* the move (so formatting is not part of the diff) and
   the script is removed in the same PR. Git's rename detection does not
   follow a section extraction (assessment F-build-14: "moving subsections
   is not a whole-file git mv"), so the PR body states that
   `git log --follow` will stop at the split for those modules and names
   the base SHA the script proved identity against.
2. **Visibility does not widen.** New files are *child modules* of the
   file they came from (`transcode.rs` stays, `transcode/producer/spawn.rs`
   is `mod producer { mod spawn; }` declared from it — the same shape as
   `live_tv.rs` + `live_tv/dvr.rs`). A child can name its ancestors'
   private items, so nothing needs `pub(crate)` to compile; an item the
   parent needs back from a child is `pub(super)`. Items other crates or
   other modules already reach keep their current visibility at their
   current path through `pub(crate) use child::Item;` in the parent —
   the "re-export" of the review, but only for paths that exist today.
   The PR body lists every `pub(super)` added; a reviewer can count them.
   This is how F-stream-15's "exposing everything pub(crate) can weaken
   boundaries" is honoured.
3. **Standing gates unchanged, plus a name list.** `make unit` passes;
   `cargo test -p plurxd --bin plurxd -- --list | sort` is identical
   before and after the PR (test names encode their module path, so a
   test moved into `transcode::tests::foo` from an inline module must be
   declared so the path is unchanged, or the rename is listed in the PR
   body); `validation/regressions.d` coverage points that name test paths
   still resolve (`make validation-lint`).

### 3.2 `transcode.rs` → `transcode/` (ranges verified at `88a3957a`)

Order matters: each row is one PR unless marked, and the first rows are
the ones with the fewest cross-references.

| # | New module | Moves (from `transcode.rs`) | Verified lines |
|---|---|---|---|
| T1 | `transcode/tests/*.rs` | `pub(crate) mod tests` split by the section comments it already has; `pretranscode_renewal_tests` → `transcode/tests/pretranscode_renewal.rs` | `27574-47096`, `11357-11403` |
| T2 | `transcode/rolling/segment_index.rs` | `SegmentVisibility`, `SegmentMeta`, `SegmentIndex`, `ReadyCoverage`, `parse_playlist` | `723-1330` |
| T3 | `transcode/rolling/flow.rs` | `Ahead`, `AheadLimits`, `AheadHoldReason`, `AheadHold`, `RollingScratchReservation`, `FlowEvaluation`, `FlowInputs`, `SuspendedAt` (not `apply_ahead_window`, which stays a manager method until T12) | `1331-1755` |
| T4 | `transcode/producer/spawn.rs` | `FfmpegDescriptors`, `WindowsPathHandoff`, `FfmpegProgressObserver`, `DiagnosticObservation`, `spawn_ffmpeg`, `spawn_ffmpeg_pipe`, `configure_ffmpeg_runtime`, `is_progress_line` | `1755-2410` |
| T5 | `transcode/rolling/prepublication.rs` | `PrepublicationTranscodeRetry`, `PrepublicationCopyRetry`, `PrepublicationStartSettlement`, `OfflineRecoveryState` | `2457-3050` |
| T6 | `transcode/rolling/retirement.rs` | `RollingRetirementParticipation/Outcome/Settlement/Ticket/Context`, `RetiredPresentation`, published-failure cleanup | `3047-4540` |
| T7 | `transcode/producer/attempt_child.rs` | `AttemptChild`, `AttemptChildTerminal`, `AttemptChildCommand`, `LifecycleTestPause`, `signal_owned_pid`, `CachedLocationIdentity`, `CacheOfferVerdict`, `FrozenHlsPresentation` (the last three move with the region because they are used inside it; T11 may move them again) | `5168-5843` |
| T8 | `transcode/rolling/publication.rs` | `PreparedSharedCacheRead`, `RollingTerminalResultReceipt/Identity/Operation`, `ServedPlaylistSnapshot`, `RollingPublicationClock`, `publication_cycle` | `5844-6020`, `6625` (+ the `Session` methods it calls, moved with T9) |
| T9 | `transcode/rolling/session.rs` | `Session` + `impl Session` | `6019-8426` |
| T10 | `transcode/response.rs` | `SegmentFile`, `MediaResponseOwner`, `MediaResponsePublication`, `MediaResponseAuthorization`, `SegmentDelivery`; the manager methods `authorize_response_publication`, `commit_authorized_media`, `response_owner_is_live` move in T12 | `8426-9110` |
| T11 | `transcode/session_request.rs`, `transcode/cluster_adoption.rs`, `transcode/requests.rs` | `StartInfo`, `SessionInfo`, `DeliveryCandidate`, `SessionRequest`, `Presentation`, `SessionKind`; `ClusterSessionStart`, `ClusterReplacementGuard`, `SessionAdoptionToken`, `SessionReleaseGate*`; `RequestEntry`, `RequestClaim`, `Claimed` | `9540-10410`, `9581-9745`, `10409-10560` |
| T12 | `transcode/pretranscode.rs`, `transcode/rate_control.rs`, `transcode/manager/*.rs` | `BoundPretranscodeSource`, `PretranscodeFence`, `PortableProduction`, `ResumedParts`, `ValidatedPart`, `ArtifactQualificationReadiness`, `GenerationObservation`; `RateControlSnapshot`, `normalize_rate_control_request`; then `impl TranscodeManager` cut along its own section comments into `manager/{construct,plan,create,control,publication,cache,retirement}.rs` as separate `impl TranscodeManager` blocks (Rust allows many) | `10563-12380`, `12485-12565`, `12942-26776` |

`probe_media_origin` (`:9236`) is §2.1 of the review's territory (its
output is currently empty because of the unpiped helper); it moves with
T12's `plan` block and is not touched otherwise.

### 3.3 `http/hls.rs` → `http/hls/` (verified at `88a3957a`)

| # | New module | Moves | Verified lines |
|---|---|---|---|
| H1 | `http/hls/tests/*.rs` | `mod tests`, split along its existing section banners; the `crate::transcode::tests::activate_control_route` callers keep that path | `14194-28507` |
| H2 | `http/hls/playlist.rs` | `playlist`, `playlist_local_before`, `master_playlist_response*`, `video_playlist*`, `exact_hls_context*`, the codec-string helpers (`dolby_vision_codec_from_init`, `hevc_codec_from_init`, `should_serve_high_tier_media_playlist`, `advertises_dolby_vision`, `strip_unadvertised_dolby_vision`, `normalize_high_tier_hevc_init`), `video_frame_rate` (test-only) | `10142-10597`, `11471-12070` |
| H3 | `http/hls/subtitles.rs` | `subtitle_playlist*`, `complete_subtitle_playlist_response`, `subtitle_vtt*`, `await_window_publication`, `slice_webvtt`, `language_name`, `subtitle_name`, `title_marks_forced`, `negated_before`, `track_is_forced` (the review's note that this "belongs beside `plurxd::subtitles`" is recorded; it stays under `http/hls/` in the move PR because moving it across the HTTP boundary is a redesign) | `10598-11470`, `12072-12680` |
| H4 | `http/hls/segment.rs` | `segment`, `vod_segment_response*`, `driven_local_body`, `session_file`, range/etag helpers, `bound_admitted_media_body`, `complete_buffered_response_before`, `authorize_response_publication`/`commit_authorized_media` (the HTTP-side wrappers), the response-completion slots | `9570-10141`, `12683-14193` |
| H5 | `http/hls/create.rs` | `CreateSession`, `create`, `create_for_library_channel`, `create_with_purpose` (1,154 lines — moved whole, not split), `plan_review_for` and the review helpers, `admit_restart`, `resolve_height`, `resolve_plan`, `validate_hevc_copy_transport`, the activation replay/settlement family | `566-4050` |
| H6 | `http/hls/release.rs` | `delete*`, `release_*`, `end_media_session_for_release`, the injected-release fault helpers (they are `#[cfg(test)]` and move with their subject) | `4051-4610` |
| H7 | `http/hls/status.rs`, `http/hls/control.rs`, `http/hls/preparation.rs` | `start`, `status`, `status_local_before*`; `control`, `control_inner`, `control_local*`, terminal-ack persistence; the prepared-successor family (`plan_preparation_candidate`, `process_preparation_candidate`, `stage_prepared_successor*`, `retire_prepared_worker`, the registries and fault hooks) | `4550-4677`, `4635-6770`, `7552-9385` |
| H8 | `http/hls/relay.rs` | `relay_if_remote`, `relay_status_requires_reclassification`, `relay_local`, `pin_shared_session_for_local_start`, `settle_media_session_request_claim` | `82-260`, `3926-4164` |

### 3.4 `vodserve.rs` → `vod/` (verified at `88a3957a`)

| # | New module | Moves | Verified lines |
|---|---|---|---|
| V1 | `vod/tests/*.rs` | `mod tests`; the `include!("vodencode_tests.rs")` at `:8473` becomes `#[path = "../vodencode_tests.rs"] mod vodencode;` or the file moves under `vod/tests/` (choose in the PR; the test names must not change — rule 3) | `8092-15011` |
| V2 | `vod/discovery.rs` | `ObsoleteEncodedGeneration`, `EncodedGenerationScanner`, `publish_encoded_process_marker`, `reconcile_obsolete_encoded_generations` | `150-240`, `5236-5310` |
| V3 | `vod/marker_prewarm.rs` | `Marker*` types and `MarkerPrewarmLedger` impl; `retire_completed_marker_prewarm`, `fence_marker_prewarm_before_room`, `decide_with_marker_prewarm`, `update_marker_prewarm_dispatch`, `clear_marker_prewarm_dispatch`, `credit_marker_prewarm_publication`, `apply_marker_prewarm_control`, `stored_marker_destinations` | `787-1329`, `6532-6735`, `7388-7448`, `7657-7767` |
| V4 | `vod/session.rs` | `Rendition`, `IdentityState`, `Session`, `Reader`, `DemandWake`, `MaterializeClock/Demand`, `TerminalCleanup*`, `HeadChildOwner`, `VodPreparationGate`, `RenditionAttachment`, `TerminalEvictionCandidate` | `660-786`, `1330-2077` |
| V5 | `vod/serve.rs` | `serve_init`, `serve_segment`, `blocked_wait`, `open_materialized`, `open_ready`, `reader_window` | `4973-5235`, `7789-7824` |
| V6 | `vod/control.rs` | `control`, `control_with_terminal` and the control helpers | `4151-4630` |
| V7 | `vod/admission.rs` | `try_admit`, `purge_if_dormant`, `eviction_windows`, `playback_demands` | `5968-6165`, `6457-6531` |
| V8 | `vod/driver.rs` | `spawn_driver`, `retire_failed_rendition`, `driver_pass`, `spawn_generation`, `recipe_engine_is_current`, `recipe_program`, `recipe_pipe_args`, `reopen_encoded_audio`, `attach_recipe_descriptors`, `attested_source_setup`, `replace_inputs_with_attested_descriptor`, `run_generation`, `establish_or_verify`, `on_generation_end`, `on_init_drift`, `record_failure`, `RenditionFailure`, `classify_failure`, `RenditionSink` + its `vodgen::Sink` impl | `6166-6456`, `6735-7530` |
| V9 | `vod/init.rs` | `regenerate_init_head`, `read_regenerated_head_before`, `read_muxer_init_bounded`, `read_muxer_init`, `StoredIdentity`, `load_identity`, `store_identity`, `sync_file` | `7846-8090` |

`VodServe`/`Shared` (`:2078-2151`) and `impl VodServe` (`:2151-5235`,
minus the rows above) stay in `vodserve.rs` as the façade; `rendition_key`
(`:7543`) and the plan helpers (`:7531-7656`) go to `vod/plan.rs` with V8.

### 3.5 Spawn unification (behaviour change, its own PR: §5.5)

After T4, `vodserve::spawn_generation` and `regenerate_init_head` call
`transcode::producer::spawn::configure_ffmpeg_runtime` (and the Windows
job helper) on their `Command`, keeping their own descriptor attachment,
`kill_on_drop`, stderr ownership and reaping exactly as they are. The
progressive path (`stream.rs`) adopts the strict `is_progress_line` and
the loose copy is deleted. What changes on the fleet: the VOD producer
gains `XDG_CACHE_HOME` (fontconfig cache) and, on Windows, job-object
membership. The assessment's F-stream-13 constraints are the tests:
inherited descriptors still arrive (`attach_recipe_descriptors` test),
cancellation still kills and reaps the exact child
(`encoded_vod_held_capacity_keeps_cached_gets_open_and_rechecks_seek_after_reap`,
`vodencode_tests.rs:492`), the 8 KiB stderr tail still reaches the
failure record, and a `filter_units=` diagnostic on the progressive path
is now logged rather than parsed as progress (new unit test with the real
line from the F-stream-12 finding).

### 3.6 Registry unification — evaluated later, not planned

Not in this plan's milestones. What the evaluation must answer before
anyone opens a PR, written here so the decision is not made by whoever is
in the file:

- The two `Session` types have different lifetimes: rolling sessions are
  *retained* after the producer ends (retirement, `RETENTION_SECS`,
  `RetiredPresentation`), VOD renditions are shared across sessions and
  survive a viewer; a viewer's session in `VodServe` is a `Reader` on a
  `Rendition`. One map cannot hold both without a sum type whose arms
  have different eviction and ownership rules.
- The four parallel methods (§2.2) are parallel in *name*; their bodies
  fence different things (`response_owner_is_live` in `transcode.rs`
  checks the retained presentation's owner; in `vodserve.rs` it checks
  the reader's `ResponseOwner` against the rendition). A shared trait
  would have to be honest about which authority each answers from.
- Lock order: `TranscodeManager::commit_authorized_media` takes
  `self.sessions.lock()` three times per commit (appendix F-stream-15);
  `VodServe` locks `shared.sessions` and per-rendition mutexes. A unified
  registry changes lock order on the segment hot path — exactly the class
  of change the month's "close … races" fixes were about.
- Risks: a new race class on the segment path; loss of the VOD-specific
  admission/eviction model (`try_admit`, `purge_if_dormant`); a bigger
  diff than every move PR combined. Benefit: one place for
  `promote_prepared_session` and friends. The evaluation is a written
  comparison with these four points measured (lock hold times from the
  existing `TimedClient`-style instrumentation, or added first), and a
  decision by Paul. If the answer is "no", the duplication is documented
  as intentional in a module-level comment on both registries.

### 3.7 `ControlState::accept_at` → pure step (behaviour-preserving)

Target signature:

```rust
pub(crate) fn accept_step(
    state: &ControlState,
    now: Instant,
    request: ControlRequestView<'_>,   // generation, owner_epoch,
                                       // client_instance_id, sequence,
                                       // ControlAcceptance
) -> Result<(ControlState, Disposition), ControlStateError>;
```

where `Disposition` is exactly today's
`(ControlDisposition, u64, ControlAction, ClientPlatform, bool)` tuple
given a name, and `accept_at` becomes
`let (next, disposition) = accept_step(self, now, view)?; *self = next;
Ok(disposition.into_tuple())`. The 14 rejection sites keep their **order**
and their variants; the table test enumerates them in that order with
one fixture per site and asserts the variant *and* that the state is
unchanged on rejection (which the mutable version does not guarantee
today — if the table test finds a site that mutates before rejecting,
that is recorded as a finding and preserved, not fixed, in this PR).
Duplicate-sequence semantics (`N−1` after `N`, and `N` with a different
body, both `StaleSequence`) are two rows of the table. The 250 ms rate
floor is a row, and the PR body says what the assessment said: the floor
does not prove concurrent requests cannot reorder. Redesign of the
phases (an explicit phase enum) is a *later* PR under the same table
test, per the assessment: "preserve behavior first, then redesign phases
under existing lifecycle tests".

### 3.8 `membership.rs` — `NodeLifecycle` as a checked projection first

The assessment refused a flat enum as the product state. So the first
step is not a transition table; it is a **projection** function
`fn observed_lifecycle(row: &ClusterNodeRow, capabilities: &[String],
promotion: Option<&PromotionRow>, maintenance: Option<&MaintenanceRow>)
-> LifecycleView` that computes, from the same rows the predicates read,
a struct of *independent* dimensions — `membership: {Absent, Learner,
Voter}`, `readiness: {NotReady, Ready}`, `maintenance: {None, Requested,
Acknowledged}`, `removal: {None, InProgress{attempt}, Tombstoned}`,
`promotion: {None, Barrier{index}, Joint}` — and a test asserting the
projection agrees with `capability_ready_predicate`,
`node_is_tombstoned` and `local_node_is_committed_voter` on every row
shape the existing tests construct. Only once the projection has agreed
with production for a release does a `transition(view, event) ->
Result<(view, effects)>` get written for the *joins* and *removals*
first, with `remove_voter`'s ambiguity reconciliation (`:5248-5420`)
expressed as effects that the manager still executes in the same order.
The etcd shape is the reference; the joint-consensus interval (`:769-770`)
is an explicit `promotion: Joint` state, never collapsed.

### 3.9 Test-seam migration

**Census script** (checked into `validation/cfg_test_census.py` by §5.1;
reproduced here so the numbers in §2.6 can be re-run):

```python
#!/usr/bin/env python3
"""Classify every `#[cfg(test)]` in crates/*/src by what it gates."""
import collections, pathlib, re, sys
ROOT = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ".")
CLASSES = [
    ("module", re.compile(r"^\s*(pub(\(crate\))?\s+)?mod\s+\w+\s*[;{]")),
    ("use", re.compile(r"^\s*(pub(\(crate\))?\s+)?use\s")),
    ("type", re.compile(r"^\s*(pub(\(crate\))?\s+)?(struct|enum|type|trait|const|static)\s")),
    ("impl", re.compile(r"^\s*impl\b")),
    ("fn", re.compile(r"^\s*(pub(\(crate\))?\s+)?(async\s+)?fn\s")),
    ("field", re.compile(r"^\s*(pub(\(crate\))?\s+)?\w+\s*:\s*[^=]")),
    ("statement", re.compile(r"^\s*(let|if|match|for|while|loop|return|\w+[\.\(]|\{|\*)")),
]
def classify(line):
    for name, rx in CLASSES:
        if rx.search(line):
            return name
    return "other"
total, per_file = collections.Counter(), collections.defaultdict(collections.Counter)
for path in sorted(ROOT.glob("crates/*/src/**/*.rs")):
    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    for i, line in enumerate(lines):
        if not re.match(r"^\s*#\[cfg\(test\)\]\s*$", line):
            continue
        j = i + 1
        while j < len(lines) and (lines[j].strip().startswith(("#[", "//")) or not lines[j].strip()):
            j += 1
        cls = classify(lines[j] if j < len(lines) else "")
        total[cls] += 1
        per_file[str(path.relative_to(ROOT))][cls] += 1
print("class,count"); [print(f"{c},{n}") for c, n in total.most_common()]
print("file,field,statement")
for rel, c in sorted(per_file.items(), key=lambda kv: -(kv[1]["field"] + kv[1]["statement"]))[:12]:
    print(f"{rel},{c['field']},{c['statement']}")
```

**Migration rule.** Each `field`/`statement` seam that pauses, delays or
injects a fault on a lifecycle path becomes a call on a `Hooks` trait
object held once per owner (`Session`, `AttemptChild`,
`RollingRetirementSettlement`, `Encoding`, `DecodeFactSource`), with
methods named for the point (`before_await_settled`,
`after_authorization_signal`, `after_flow_reservation`,
`before_reap_terminate`, `before_admission`, `initial_identity_delay`,
`final_identity_delay`). Production installs `NoopHooks` (a unit struct;
every method is an empty `async fn` or returns `Duration::ZERO`); tests
install the pausing implementation that today's barriers become. The
struct then has the same layout in both builds (one `Arc<dyn Hooks>`), and
the `.await` on a hook is a cancellation point in both builds — the
no-op future is `Ready` and costs one poll. Where a seam is not a pause
but an *alternate implementation* (`#[cfg(not(test))]` twins), the
production body becomes the only body and the test variant becomes a
hook return value. The `fail` crate is the alternative for statements
that are pure fault injection (`fail_point!("transcode.before_reap")`),
enabled by a cargo feature the release build never sets — the
`scratch-fault-injection` shape. The plan prefers the trait where the
seam is an ordering pause (it keeps the deterministic race tests
readable) and `fail` where it is a one-shot fault.

What is *not* migrated: `fn`, `type`, `impl`, `use` and `module` classes
(test helpers are fine as `#[cfg(test)]`), and any seam whose only effect
is a counter (`decode_facts.rs:1738 test_counter`) — those are converted
to hooks only if they sit on a struct whose layout matters.

Honesty about what this buys, from the assessment: "injected no-op hooks
… do not make scheduling identical to an activated blocking failpoint".
The layout and the *set* of cancellation points become identical; the
timing under an activated pause is still a test artefact. That is why the
deterministic race tests are kept, not replaced, and why §5.8 adds one
shipped-binary test per migrated owner (a release-profile integration
test that exercises the path with `NoopHooks`, proving the production
shape runs the same test scenario to completion).

## 4. Guardrails (non-goals)

1. **A move PR changes no behaviour.** No lock order, `.await`, error
   string, log line, metric or constant changes. The identity script is
   the proof; a PR whose script prints a hunk is not a move PR.
2. **No `pub(crate)` widening to make a move compile** (rule 2). Child
   modules and `pub(super)` only; the PR body lists the `pub(super)`s.
3. **Spawn unification, `accept_step`, `NodeLifecycle` and seam migration
   are separate PRs with new tests**, never folded into a move. (F-stream-15,
   F-core-10, F-sc-12, F-build-3.)
4. **Registry unification is not planned.** §3.6 is an evaluation
   checklist; a PR needs Paul's decision on its written result first.
5. **Do not delete deterministic race tests** when migrating seams. A
   migrated seam keeps its test, expressed through the hook.
6. **Do not present the 40 % fix share as a structural proof.** F-hist-13:
   touches across overlapping files are not deduplicated in that number;
   the plan's justification is reviewability and the byte-identity
   method, not that number.
7. **Do not treat the decomposition as a prerequisite for other fixes.**
   The assessment says it is not; §5's ordering leaves `main` mergeable
   between every milestone, and any §2 fix from the review may land in
   between — the mover rebases, the identity script re-proves.
8. **Do not add a `clippy::too_many_lines` lint or a file-size gate in
   these PRs.** F-build-14: unproved benefit; a separate decision.
9. **Do not move `http/hls/subtitles.rs` out of the HTTP tree** in the
   move PR, and do not unify playlist parsers here (L8; grammar inventory
   first).

## 5. Milestones

Every milestone is one draft PR into `main` under the fast lane unless a
row says otherwise. Any other work may land between two milestones; the
next milestone rebases and re-runs its identity script against the new
base. Sizing: M0 S; M1 M; M2 M; M3 M; M4 S; M5 S–M; M6 M; M7 M; M8 L over
several PRs.

### 5.1 M0 — census, baselines and the identity script

Add `validation/cfg_test_census.py` (§3.9) and a
`scripts/split-identity` template (parameterised by base SHA, source
file, and ordered module list) that later PRs copy and delete. Record
`cargo test -p plurxd --bin plurxd -- --list | sort > /tmp/plurxd-tests.base`
in the PR body's procedure. No source moves.

Acceptance: `python3 validation/cfg_test_census.py .` prints the §2.6
table (±5 per class, since the script is line-oriented); the identity
template run against an unchanged file prints `OK against 88a3957a`.

### 5.2 M1 — inline tests out (T1, H1, V1; three PRs or one, reviewer's call)

Move the five test modules to `src/<mod>/tests/*.rs`, declared as
`#[cfg(test)] mod tests;` (with `#[path]` where the directory name
differs), splitting each along its existing section banners into files
under 3,000 lines. `transcode::tests` stays `pub(crate)` for `hls.rs`'s
callers. `vodencode_tests.rs`/`vodencode_manager_tests.rs` keep their
names (VOD-ENCODING.md links them).

Acceptance: `cargo test -p plurxd --bin plurxd -- --list | sort` is
byte-identical to the M0 baseline; identity script `OK` for each file
(the product half is untouched, so the script diffs the product half
only and lists the test files' total line count against the removed
module); `make unit` green.

### 5.3 M2 — `transcode.rs` leaf regions (T2–T7)

One PR per row of §3.2 T2–T7. Each: `git mv`-free section extraction,
child module, `pub(super)` list, identity `OK`.

Acceptance per PR: identity `OK against <base>`; test name list
unchanged; `make unit` green; `grep -c "pub(crate)" crates/plurxd/src/transcode.rs`
not increased by the PR (the diff shows only `pub(super)` additions in
the new files).

### 5.4 M3 — `transcode.rs` session, response, request and manager (T8–T12)

Same rules; T12 is the largest and may be two PRs (`pretranscode` +
`rate_control`, then the `impl TranscodeManager` cut). The manager cut
keeps one `impl TranscodeManager` block per file and *no* method changes.

Acceptance: as §5.3; additionally `wc -l crates/plurxd/src/transcode.rs`
under 2,000 (declarations, re-exports, constants and the manager struct
only).

### 5.5 M4 — spawn unification (behaviour change)

§3.5. Tests named there; PR body records a media1 run of one text-burn
VOD session before and after showing the fontconfig cache path in the
child's environment (`cat /proc/<pid>/environ | tr '\0' '\n' | grep XDG`).

Acceptance: `cargo test -p plurxd vodencode_tests::encoded_vod_held_capacity --
--ignored` green; new `stream::tests::progress_grammar_rejects_bsf_diagnostic`
green; `make unit` green.

### 5.6 M5 — `http/hls.rs` (H2–H8) and `vodserve.rs` (V2–V9)

Same rules as M2, one PR per row, interleavable with anything.

Acceptance: as §5.3 per PR; `wc -l` of each parent file under 2,000 at
the end.

### 5.7 M6 — `accept_step` (behaviour-preserving redesign)

§3.7. Table test `playback_control::tests::accept_step_rejection_order`
with 14 + 2 rows.

Acceptance: `cargo test -p plurxd playback_control::` green; the table
test's row order matches the `return Err` order in the pre-PR
`accept_at` (the PR body pastes both lists side by side); no
`ControlStateError` variant or message text changed
(`git diff -- crates/plurxd/src/playback_control.rs | grep -c "ControlStateError::"`
shows moves, not edits).

### 5.8 M7 — `NodeLifecycle` projection

§3.8, projection and agreement test only; the transition function is a
later PR gated on one release of agreement.

Acceptance: `cargo test -p plurx-core cluster::membership::tests::lifecycle_projection_agrees`
green over every row fixture the existing tests build; no SQL constant
changed.

### 5.9 M8 — seam migration, owner by owner

Order: `RollingRetirementSettlement` (1 field, 1 statement pair) →
`AttemptChild` (3 fields) → `Encoding.admission_pause` →
`DecodeFactSource` delays → the `playback_control.rs` seams → the
`hls.rs` fault helpers. One owner per PR; each PR migrates the seam,
keeps the race test through the hook, and adds a release-profile
integration test running the same scenario with `NoopHooks`.

Acceptance per PR: `python3 validation/cfg_test_census.py .` shows the
owner's `field` and `statement` counts at zero; the race tests it names
still pass; `cargo test --release -p plurxd <owner>_shipped_shape` green;
`make unit` green.

## 6. Verification and rollout

- Fast lane: `make unit` on every PR; `make validation-lint` (regression
  coverage points name test paths); `make operations-check` when a doc
  path changes.
- Per move PR: the identity script output pasted in the PR body; the
  test-name diff (empty); the `pub(super)` list.
- Per redesign PR: the named focused test command and its output.
- Fleet: no rollout step for move PRs beyond the ordinary deploy; the
  binary is the same code. M4 (spawn) is the one milestone with a fleet
  observation (§5.5). Nothing in this plan needs a setting, a metric or
  a device run; if a milestone finds it does, that milestone stops and
  becomes a redesign PR with its own plan.
- Rollback: revert the PR. Move PRs revert cleanly by construction;
  M4 reverts to the three spawn paths.

## 7. Open questions

1. Whether `transcode::tests` should stay `pub(crate)` after H1 or the
   four `hls.rs` callers should get their own fixture; the move keeps the
   path, the question is for the H1 reviewer.
2. Whether the `impl TranscodeManager` section comments are complete
   enough to cut T12 into seven files without a method landing in the
   wrong owner; if not, T12 stops at `manager.rs` as one file and the
   further cut becomes its own PR.
3. How many of the 153 `statement` seams are pauses (hook candidates)
   versus one-shot faults (`fail` candidates); the `syn` pass in M8's
   first PR answers it, and the split of the M8 PRs follows the answer.
4. Whether Paul wants the §3.6 registry evaluation written at all this
   quarter, or the duplication documented as intentional now.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| | | | | | |
