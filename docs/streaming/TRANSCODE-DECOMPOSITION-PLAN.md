# Transcode decomposition — behaviour-preserving moves first, redesign later

**Status:** executing in draft [PR #543](http://192.168.4.7:3000/noirr/plurx/pulls/543) from M7 onward (M0 to M6 came in through [PR #425](http://192.168.4.7:3000/noirr/plurx/pulls/425) and [PR #511](http://192.168.4.7:3000/noirr/plurx/pulls/511); M4 through S-05) · **Executes:** §4.1, §4.2, §4.9, F-stream-15,
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

**Decision D-M7 (2026-09-25, taken by the executing session under Paul's
delegation of reasonable design decisions; Paul can overturn it).** The
row types above did not exist when M7 opened, and the three agreement
points are not Rust predicates over rows: `capability_ready_predicate` is
SQL evaluated inside Raft transactions, `node_is_tombstoned` a consistent
SQL count, and `local_node_is_committed_voter` a Raft metrics read. Two
designs were open: (a) a row-typed read model that production reads
through, or (b) row types used only by the projection, with a test that
evaluates the production SQL itself over fixtures. **Chosen: (b), the
smaller one.** (a) changes production read paths, which M7 excludes
("projection and agreement test only"). What (b) is, concretely:

- `crates/plurx-core/src/cluster/membership/lifecycle.rs` holds the row
  types (`ClusterNodeRow`, `CapabilityRow`, `RemovalRow` with its attempt
  ids, `PromotionRow`, `MaintenanceRow`, gathered in `NodeRows`), a
  `RaftMembershipView` built from the accessors production calls
  (`voter_ids()`, `nodes()`, `get_joint_config().len() > 1`), and
  `observed_lifecycle(NodeRows, &RaftMembershipView) -> LifecycleView`.
  Nothing in the daemon calls it.
- The test writes fixtures into the production table definitions (the
  `CREATE TABLE` statements of `MEMBERSHIP_SCHEMA` plus the additive
  columns; the triggers are left out because they need the coordinator's
  intent rows), reads the rows back into those types, and compares the
  projection with the production predicate functions and constants
  themselves, never copies.
- Production reads are unchanged. The tombstone count moved verbatim from
  an inline literal into `NODE_TOMBSTONED_SQL` so the test can run the same
  text, and `local_node_is_committed_voter` calls
  `lifecycle::committed_voter`, which is its old
  `voter_ids().any(|id| id == raft_id)` rule. After the review of #543 three
  more literals moved verbatim the same way (`NODE_MAINTENANCE_COUNT_SQL`,
  `NODE_PROMOTION_COUNT_SQL`, `EXISTING_REMOVAL_ATTEMPT_SQL`), and the
  metrics read `local_node_is_committed_voter` passes to that rule is the
  `committed_voter_ids!` macro (`membership_config.voter_ids()`, as before),
  so the test applies the same read to the `StoredMembership` values it
  builds. A macro, because `openraft` is a dev-dependency of `plurx-core`
  and production code cannot name its type. No SQL text changed.
- What the test agrees, dimension by dimension (after the review of #543;
  the first version asserted neither `maintenance` nor the attempt set, and
  compared the committed-voter verdict with the projection's own rule):
  readiness and `join` against `capability_unready_node_predicate` and
  `capability_ready_predicate`; the tombstone against `NODE_TOMBSTONED_SQL`;
  the attempt set of a removal in progress against the three reads
  production makes of it (`ROLLBACK_REMOVAL_FENCE_SQL` needs it empty,
  `BEGIN_REMOVAL_INTENT_SQL` needs one attempt in it,
  `EXISTING_REMOVAL_ATTEMPT_SQL` returns its least attempt);
  `durable_role` against `admitted_learner_nodes_sql`; a maintenance request
  against `NODE_MAINTENANCE_COUNT_SQL` and `Acknowledged` on an active node
  against `EXIT_MAINTENANCE_SQL` itself, run in a rolled-back savepoint with
  its other preconditions made true; `membership` against
  `committed_voter_ids!`. **Not agreed, because production has no reader
  for them:** `promotion`'s `Started` / `Barrier` / `Joint` split (only the
  audit row's existence is read, and that is agreed against
  `NODE_PROMOTION_COUNT_SQL`; nothing reads `barrier_index` back), the
  attempt set of a tombstoned node (the references are left behind on
  purpose and no removal read consults them again), and `Acknowledged` on a
  tombstoned node. The test pins the promotion split to this section's
  specification; that is not an agreement with production, and the
  transition PR must not treat it as one.
- Dimensions beyond the list above, because the predicates read them:
  `durable_role` (the `role` column; `admitted_learner_nodes_sql` reads it)
  and `join` (the staging row, which `capability_unready_node_predicate`
  exempts). `promotion` gains `Started` (an audit row whose `barrier_index`
  is still NULL). `Joint` is taken from the committed Raft configuration
  while the audit row exists, so the joint interval stays its own state.
- "Agreed with production for a release" is read as: the agreement test is
  in the tree and green for one release before the transition PR. No
  production shadow comparison is added, because that needs the production
  reads (a) would add.

To overturn it: choose (a), or add a shadow comparison, before the
transition PR is opened. The projection and its test stay useful either
way.

### 3.9 Test-seam migration

**Census script** (checked into `validation/cfg_test_census.py` by §5.1;
reproduced here so the numbers in §2.6 can be re-run):

```python
#!/usr/bin/env python3
"""Classify every `#[cfg(test)]` in crates/*/src by what it gates."""
import collections, pathlib, re, sys
ROOT = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ".")
CLASSES = [
    ("module", re.compile(r"^\s*(pub(\([^)]*\))?\s+)?mod\s+\w+\s*[;{]")),
    ("use", re.compile(r"^\s*(pub(\([^)]*\))?\s+)?use\s")),
    ("type", re.compile(r"^\s*(pub(\([^)]*\))?\s+)?(struct|enum|type|trait|const|static)\s")),
    ("impl", re.compile(r"^\s*impl\b")),
    ("fn", re.compile(r"^\s*(pub(\([^)]*\))?\s+)?(async\s+)?fn\s")),
    ("field", re.compile(r"^\s*(pub(\([^)]*\))?\s+)?\w+\s*:\s*[^=]")),
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
no-op future is `Ready` and costs one poll. **As built (M8's first owner,
#543):** each owner gets its own small trait named for its points, held as
`Box<dyn …Hooks>` (`RollingRetirementSettlement` holds
`Box<dyn RetirementSettlementHooks>`, production installs
`NoopRetirementSettlementHooks`), not one shared `Hooks` behind an `Arc`.
The owners' points do not overlap, and boxing the zero-sized no-op
allocates nothing. Later owners follow the per-owner shape unless their PR
records why not. **As built ([#556](http://192.168.4.7:3000/noirr/plurx/pulls/556)):** `AttemptChild` holds
`Arc<dyn AttemptChildHooks>` and `DecodeFactSource` holds
`Arc<dyn DecodeFactSourceHooks>` — `Arc`, not `Box`, because the child and
its supervisor task both hold the hooks and the source is `Clone`. Two of
`AttemptChild`'s three points (`after_signal_authorization`,
`after_flow_reservation`) are synchronous methods: they sit inside the
producer transition fence, where the old seams blocked on a
`std::sync::Barrier` and neither build has an await point. The test pauses
at those two points are a bounded `SupervisorPause`, not a barrier: the
held supervisor blocks a runtime worker thread, which a runtime drop cannot
cancel, so a race test that fails while it is held must release it (a
guard that releases on drop, including while unwinding) and a supervisor
whose test never arrives lets itself go after 10 s. The hook trait
has `Any` as a supertrait so a test can reach the pauses installed on a
child it did not construct (the test-only `AttemptChild::new` installs
them; production `new_with_job` installs the no-op). `DecodeFactSource`'s
`#[cfg(not(test))]` getter twins are gone: both builds read the delay from
the hook. The census script above now accepts any `pub(...)` restriction;
it matched only `pub(crate)`, so `pub(super) fn` lines were counted as
statements. **As built ([#562](http://192.168.4.7:3000/noirr/plurx/pulls/562)):** `Encoding`, `RollingFlowSync`,
`TerminalCleanup`, `HeadChildOwner` and `VodServingAdmission` take the
per-owner shape (`Box`, except `HeadChildOwner`'s `Arc`, shared with the
reaper task it detaches). `HookFuture` and `HookReady` moved to
`crates/plurxd/src/seam_hooks.rs` so owners outside `transcode` share them.
Those five owners' pauses are all asynchronous, so their tests use
`seam_hooks::AsyncPause`, the asynchronous counterpart of
`SupervisorPause`. Both sides are bounded by wall-clock time measured on a
helper thread, so the bound holds under `tokio::time::pause()` and never
moves a paused clock. A guard releases the owner on drop or unwind, and a
release before the owner arrives disarms the pause. `Encoding`'s hooks are
chosen where the encoding is built (Decision D-M8-E, execution log), not
supplied by a hook factory on `TranscodeManager`. **As built
([#573](http://192.168.4.7:3000/noirr/plurx/pulls/573)):** the `playback_control.rs` control group (`RollingControlHandle`,
`RollingActorRuntime`, `RollingControlActor`, `RollingActorExitFence`,
`RollingDecisionTransport`, `RollingProducerIngress`) shares one
`Arc<dyn RollingControlHooks>`, one trait for the group rather than one per
struct, because the six structs are one lifecycle: the handle's
constructor spawns the actor and executor tasks that hold the rest.
Record points (the abort handles, the start and exit notifications, the
last flow barrier) are synchronous hook methods whose no-op does nothing.
The hooks are chosen at construction, because the tasks that hold them are
spawned inside the constructor: production `spawn` and
`spawn_prepublication_*` install `NoopRollingControlHooks`, and test code
calls `_for_test` twins that install `RollingControlTestHooks` (Decision
D-M8-G). The six test-driver command variants stay `#[cfg(test)]`
(Decision D-M8-H). `StartedSessionCleanup` takes the per-owner `Box` shape; a
test replaces the hooks on the armed guard, which it owns mutably. **As built
([#579](http://192.168.4.7:3000/noirr/plurx/pulls/579)):** `Rendition` holds `Box<dyn RenditionHooks>`, chosen
where the rendition is built (production `build_rendition` installs the no-op,
the test-only `synthetic_rendition` the notifying test hooks); its two points
are records. The VOD `Shared` holds `seam_hooks::HookSlot<dyn VodSharedHooks>`
(Decision D-M8-I): tests build the registry through the production
constructors and arm it afterwards through the `Arc` its tasks share, so the
hooks cannot be chosen at construction without threading a choice through
`VodServe`'s constructors and `TranscodeManager::new`. The slot is a set-once
`OnceLock` that production never fills, so every point reads the no-op; a test
fills it with the test hooks the first time it arms a point. Its seven pauses
are `seam_hooks::PauseSlot`s (an armable one-shot `AsyncPause`); the
terminal-route override is a hook return value (`terminal_route_outcome`,
`None` in production). **As built ([#581](http://192.168.4.7:3000/noirr/plurx/pulls/581)):** the rolling `Session` holds
`seam_hooks::HookSlot<dyn SessionHooks>` and `TranscodeManager` holds
`seam_hooks::HookSlot<dyn TranscodeManagerHooks>` (Decision D-M8-I), so none of
their construction sites changed. Every `SessionHooks` point hands back a
`'static` future, so the detached retention and scratch owners take their
point where they are spawned and hold no reference to the session.
`TranscodeManager`'s seams are classified in the trait itself: two pauses, two
overrides (`forces_artifact_qualification`, `scripted_offline_outcome`), one
fault (`offline_recovery_begin_fault`), two records and one plumbing point
(`decode_fact_source`, which hands a test delay to `DecodeFactSource`'s own
hooks). The manager keeps no test-only site: its test-build decoder fallback
in `resolve_movie_plan_with_qualification` was deleted in #581's review round,
so test and release managers resolve decode capabilities from the same
inventory. **As built ([#583](http://192.168.4.7:3000/noirr/plurx/pulls/583)):** the remaining
`http/hls/` route sites take one `HlsRouteHooks` held by `AppState` in a
`HookSlot` that every clone of the state shares (Decision D-M8-J): seven awaited
points, four faults as return values (`staged_read_fault`,
`preparation_settlement_fault`, `refuses_preparation_planning`,
`release_commit_unknown`), two records, and `primes_prepared_successor`, the
return value that replaces the `#[cfg(not(test))]` stage-and-prime twin
(production primes; hooks that decline stage the durable row only, on this node,
as a selection change). The keyed module statics became `HlsRouteTestHooks`'
per-state tables. The test-only `AppState::new` installs the test hooks with
priming off, because the stage-only choice applied to every test build (Decision
D-M8-K), and `prime_prepared_successors` turns it on; `AppState::new_unhooked`
leaves the no-op, and arming a point on such a state installs test hooks that
keep priming, so a test can hold a point on the production priming path. The three delays stay delays, bounded
by the duration armed; the four rendezvous are bounded (`AsyncPause`, and a
dispatch wait that gives up after `ASYNC_PAUSE_BOUND`). Where a
seam is not a pause but an *alternate implementation* (`#[cfg(not(test))]` twins), the
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
   the proof; a PR whose script prints a hunk is not a move PR. A log
   line's *target* is part of the line and is outside what reassembly can
   see: a `#[path]` child's default tracing target is its own
   `module_path!()`, so every event a move puts in a child names its
   parent's target explicitly (`target: "plurxd::transcode"`,
   `"plurxd::http::hls"`, `"plurxd::vodserve"`), the identity script
   removes exactly those pins before comparing, and
   `split_children_log_under_their_parent_target` walks the child
   directories to keep it so (review of #511, finding 4).
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
`hls.rs` fault helpers. **Done:** `RollingRetirementSettlement` (#543),
`AttemptChild` and `DecodeFactSource` (#556; `DecodeFactSource` taken ahead
of `Encoding`, whose design question is in the execution log), `Encoding`,
`RollingFlowSync`, `TerminalCleanup`, `HeadChildOwner` and
`VodServingAdmission` ([#562](http://192.168.4.7:3000/noirr/plurx/pulls/562); five owners in one PR, each small and
each proved separately, recorded as a deviation), the `playback_control.rs`
control group and `StartedSessionCleanup` ([#573](http://192.168.4.7:3000/noirr/plurx/pulls/573); two commits,
recorded as a deviation), `Rendition` and the VOD `Shared` ([#579](http://192.168.4.7:3000/noirr/plurx/pulls/579); two
commits, recorded as a deviation), `Session` and `TranscodeManager` ([#581](http://192.168.4.7:3000/noirr/plurx/pulls/581);
each armed through a `seam_hooks::HookSlot`, Decision D-M8-I; separate
commits, recorded as a deviation), the `http/hls/` route sites behind one
`AppState`-held `HlsRouteHooks` ([#583](http://192.168.4.7:3000/noirr/plurx/pulls/583); Decisions D-M8-J and
D-M8-K). **Next:** the owners
`seam_census_of_the_workspace` still prints (premises in the execution log). One owner
per PR; each PR migrates the seam,
keeps the race test through the hook, and adds a release-profile
integration test running the same scenario with `NoopHooks`. The §7 Q3
`syn` pass is owed by the `AttemptChild` PR, not the first one (see §7).

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
   versus one-shot faults (`fail` candidates). **Answered 2026-09-26
   ([#556](http://192.168.4.7:3000/noirr/plurx/pulls/556)).** The `syn` pass is
   `crates/plurxd/src/transcode/tests/seam_census.rs`;
   `cargo test -p plurxd --bin plurxd seam_census -- --nocapture` prints
   the table. (#543's one owner, a single `Barrier` pause, was classified
   by reading.) It parses every production file under `crates/*/src`, skips
   files reached only through a `#[cfg(test)]` module or `include!`, and
   `#[cfg(test)]`/`#[test]` items, and classifies each gated statement in a
   shipping function by what it does (`#[cfg(all(test, ..))]` counts as
   gated). After this PR's two migrations, of the
   158 gated statements: **48 pause** (a wait on a rendezvous or a
   delay: `wait`, `notified`, `recv`, `acquire`, `sleep`, or awaiting a
   held receiver), **17 fault** (an injected `Err`, `?` on a
   test-only call, `panic!` or a `pending()` hang), 15 override (a
   substituted value or an early `return` of a test-supplied result, an
   alternate implementation rather than a fault), 34 record (counters,
   pushes, notifications with no wait) and 44 plumbing (a `let` that
   carries a slot to one of the others). Beside them: 97 gated struct fields
   and enum variants, 104 struct-literal initialisers and 19 `match`
   arms, each counted against its owner. (The line census's 153 was never
   this population: it counts every literal `#[cfg(test)]` line, including
   test-only files, initialisers and arms.)

   **What follows for the remaining M8 PRs.** Pauses dominate, so the
   per-owner hook trait stays the default shape. The one-shot faults are
   concentrated: 7 of the 17 are `dv_disk.rs`'s injected filesystem
   failures, one in each filesystem step of the Dolby Vision disk
   replacement (link, rename, unlink, restore, scratch removal) — one
   module, and the natural candidate for the `fail`-style cargo feature on
   the `scratch-fault-injection` precedent rather than a trait; the rest are single sites spread over `live_tv.rs`,
   `decode_facts.rs`, `http/hls/{control,release}.rs`, `offline.rs`,
   `transcode/manager/produce.rs` and `cluster/migration.rs` (beside its
   existing `PLURX_CLUSTER_ACTIVATION_FAILPOINT` env failpoint), each
   migrated with its owner. The
   `playback_control.rs` owners are mostly plumbing and arms around a few
   pauses, so they are trait migrations; the `hls.rs` "fault helpers"
   named in §5.9 are `fn`-class helpers whose consuming statements are
   mostly pauses (`release_session`, `settle_preparation_control`) with one
   fault each in `control_local_with_settlement_capacity` and
   `end_media_session_for_release`. The override and record classes are
   not lifecycle seams by §3.9's rule unless their owner's layout matters;
   they are left to their owners' PRs.

   **`dv_disk.rs` decided ([#562](http://192.168.4.7:3000/noirr/plurx/pulls/562), Decision D-M8-F; Paul can
   overturn it): no fault-injection cargo feature.** Its seven faults and
   their plumbing gate statements and module statics
   (`FINALIZE_SWAP_HOOKS`, `RENAME_REOPEN_FAILURE_HOOKS` and five more); the
   census finds no gated field or initialiser there, so the layout argument
   that justifies a hook does not apply. A cargo feature would replace
   `cfg(test)` with `cfg(feature = "…")` on the same statements, so the
   release binary would still omit them and the test binary would still
   include them. `plurxd` is a binary crate, so its unit tests see a feature
   only when every `cargo test`/`clippy` invocation passes `--features`
   (`make unit`, the fast Rust gate, the clippy and Windows lanes). That
   gate change buys nothing either binary runs, so the faults stay
   `#[cfg(test)]` one-shot faults, and `dv_disk.rs` leaves M8's list.
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
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | M0 | [#425](http://192.168.4.7:3000/noirr/plurx/pulls/425) | Pinned Rust 1.97.1 established. Census: module 183, fn 255, field 173, statement 156, type 31, impl 13, use 8, other 35 (all within the plan's ±5 bound). The parameterized identity template reports `OK` against the exact branch base/current parent source. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | M1 | [#425](http://192.168.4.7:3000/noirr/plurx/pulls/425) | Moved the `transcode`, `http::hls`, and `vodserve` inline test modules into same-path include chunks below 3,000 lines; moved `pretranscode_renewal_tests` alongside them. The exact base/head test-name lists are byte-identical at 2,454 tests. `scripts/split-identity 9deb58a2 --plan` expands each child manifest at an explicit parent marker, reverses only the five checked relative-path relocations, and reconstructs all three parents byte-for-byte; affected all-target check and one focused test from each moved region pass. The current one-plan/one-PR protocol chooses the plan's allowed combined-review form rather than three milestone PRs. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/c02_builder | M2 / T2 | [#425](http://192.168.4.7:3000/noirr/plurx/pulls/425) | Moved the complete segment-index leaf region to `transcode/rolling/segment_index.rs`. Parent access is restored only with 40 checked `pub(super)` additions; no crate visibility widened. The same marker-expanded identity command strips exactly the structural child header and those 40 additions, then reconstructs the complete base parent byte-for-byte. Pinned all-target check and the shrink, concurrent-observation, and prune/append regressions pass. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M2 / T3-T7 | [#511](http://192.168.4.7:3000/noirr/plurx/pulls/511) | `c9dcbef4f`: five contiguous leaf regions moved verbatim into `transcode/rolling/flow.rs`, `producer/spawn.rs`, `rolling/prepublication.rs`, `rolling/retirement.rs` and `producer/attempt_child.rs`, plus `producer/prepublication_executor.rs` for the base region no §3.2 row mapped (named T6b). Parent reach restored with `pub(super)` only (196 additions, counted per child) and two re-exports at their existing `pub` visibility. `scripts/split-identity 0e2c3fd47 --moves` reports `OK` for `transcode.rs`. Test-name list byte-identical to the base (2,780 names). |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M3 / T8-T12 | [#511](http://192.168.4.7:3000/noirr/plurx/pulls/511) | `09850da65`: session, response, request and manager regions moved into `rolling/publication.rs`, `rolling/session.rs`, `response.rs`, `hls_codecs.rs`, `cluster_adoption.rs`, `session_request.rs`, `requests.rs`, `pretranscode/{source,parts}.rs`, `rate_control.rs`, `rolling/retention.rs`, `ladder.rs`, `test_support.rs` and `manager/*.rs`. `transcode.rs` was 1,802 lines at this commit (1,810 after the main merges; acceptance: under 2,000). §7 Q2 answered: the `impl TranscodeManager` block had one section comment, so it is cut at method-group boundaries into ten contiguous impl blocks, order preserved, no method changed. Items reached as `crate::transcode::X` keep that path through glob re-exports, which clamp each item to its own visibility, so none widens. Identity `OK` against `0e2c3fd47`. **Deviation, recorded after the review of #511 (finding 5):** two items §3.2 assigns to M3 stayed in `transcode.rs`. `StartInfo` (T11, target `session_request.rs`) sits between the `hls-codecs` and `cluster-adoption` markers, and `probe_media_origin` (§3.2: moves with T12's `plan` block) sits with its inline test between the `response` and `hls-codecs` markers, far from the `impl TranscodeManager` region. A child here is one contiguous region reassembled at one marker, so moving either into its destination would reorder the parent and needs its own identity recipe; neither was moved. The parent also still holds about 30 free functions (error-string constructors, `session_log_id` and the ffmpeg log helpers, the live-recovery counters, `probe_media_origin`) and several structs (`Progress`, `BoundPlanCaller`, `RecentMarkerAmbiguityLedger`, `HttpWaitLedger`, `TranscodeMetrics`, `CodecQualificationMetrics`, `MediaNodeRuntime`, `MediaOfferProbe`, `RollingTerminalAdmission`), so M3's qualitative acceptance ("declarations, re-exports, constants and the manager struct only") is **not met**; the line criterion is. Moving them is a follow-up move PR, not done here. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M4 | [#511](http://192.168.4.7:3000/noirr/plurx/pulls/511) | Not executed in this plan. S-05 [FFMPEG-SPAWN-UNIFICATION](FFMPEG-SPAWN-UNIFICATION.md) delivered §3.5 first ([#415](http://192.168.4.7:3000/noirr/plurx/pulls/415), `e12f0c023`, `fe6067748`): VOD generation, head regeneration and progressive remux spawn through `producer_spawn::spawn`, which applies `configure_ffmpeg_runtime`, and one progress-key classifier lives in `plurx_core::transcode::progress`. This plan's `transcode/producer/spawn.rs` is the rolling path's move and changes none of that. **needs:** the §5.5 fleet observation, which is S-05's M3 fleet check and is recorded once, there. GPT prompt: *On media1 (Docker) after deploying a build that contains #415: start Harbor Lights as an encoded VOD session with the text subtitle track burned in; paste `cat /proc/$(pgrep -n -f 'ffmpeg.*dev/fd/3')/environ \| tr '\0' '\n' \| grep -E 'XDG_CACHE_HOME\|AV_LOG_FORCE_NOCOLOR'` (both must be present), and the time to first segment for this start and a second start of the same title; then start a progressive remux from the web client and paste the same grep for its ffmpeg. Record the result in S-05's and this plan's execution logs through one evidence-only docs PR.* |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M5 / H2-H8, V2-V9 | [#511](http://192.168.4.7:3000/noirr/plurx/pulls/511) | `0dfe5b1c8`: every product region of `http/hls.rs` (now 191 lines) and `vodserve.rs` (728 lines at this commit, 751 after the main merges) moved verbatim into `http/hls/*.rs` and `vod/*.rs`; `impl VodServe` is cut at method boundaries into six contiguous impl blocks under `vod/serve/`. Three relocations, each reversed exactly by `scripts/split-identity 0e2c3fd47 --moves`, which reports `OK` for both parents: hls.rs's `pub(super)` written `pub(in crate::http)` (or `pub(in crate::http::hls)` inside `plan_derivation`) in a child (the same 21 sites), one extra `super::` on 13 sibling paths, and the parent-reach `pub(super)`s counted per child. `http/hls/subtitles.rs` did not move (guardrail 9). |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | Source scans | [#511](http://192.168.4.7:3000/noirr/plurx/pulls/511) | `37375df70`: `validation/rust_modules.py` `module_source(path)` reassembles a split parent (children at their `// split:` markers, split plumbing removed) so the Python inventories that pin anchors or counts in the three parents read the same text as before; no pinned count or anchor changed. `9bb321474`: two Rust scans updated - `the_roster_reader_has_exactly_these_callers` walks every non-test child under `http/` (proved by planting a roster read in `http/hls/status.rs`: the test fails naming it), and the hls cancellation-reason scan accepts the rustfmt-wrapped forwarding signature's trailing comma. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M6 | [#511](http://192.168.4.7:3000/noirr/plurx/pulls/511) | `043f8751a`: `accept_step(&ControlState, Instant, ControlRequestView) -> (ControlState, Result<Disposition, ControlStateError>)`; `accept_at` is the step plus `*self = next`, and the fence body moved unedited into `accept_in_place`. **Deviation from §3.7's signature, recorded as the finding §3.7 asks for:** rejection is not atomic, so the step returns the next state on both arms. Three sites change state before refusing and are preserved, not fixed: a first packet adopts the generation before its sequence or platform is judged; an owner-epoch advance resets the sequence space and registers the client before the prepared-successor observation refuses; a request's ask (`desired_digest`) is recorded before an `Unavailable` observation refuses. `accept_step_rejection_order` has 17 rows in `return Err` order: the 14 `return Err` sites, the two reachable `?` sites (the third, a missing platform after registration, is unreachable) and the rollover finding; duplicate sequences are two rows and the 250 ms floor is one. The floor orders one client's packets; it does not prove concurrent requests cannot reorder. `accept_step_matches_the_in_place_fence_on_accepted_exchanges` pins equivalence on accepted, replayed and epoch-advancing exchanges and that the step never writes its input. No `ControlStateError` variant or text changed. Proof: making `accept_at` drop the stepped state fails the table test. **Review of #511 (finding 1):** a row that breaks one fence shows the site is reachable, not that it runs before its neighbour, and swapping the platform check with the sequence floor, or lifting the rate floor above the replay block, left both M6 tests green. The table test now also sends, for each of the eight adjacent pairs one packet can break together, a packet that breaks both and asserts that the earlier site answers (variant and residue), plus one row pinning that the ask lands after the rate floor; both review mutations fail it (run). The adjacent pairs whose order cannot be observed (exclusive branches, or `StaleClient` from both with nothing left behind) are listed in the test's doc comment. The `accept_step` doc comment no longer names a fourth non-atomic site (finding 2): a recorded terminal acknowledgement skips the observation, so it never precedes an `Unavailable`. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M7 | [#511](http://192.168.4.7:3000/noirr/plurx/pulls/511) | Not opened. §3.8 names `ClusterNodeRow`, `PromotionRow` and `MaintenanceRow` inputs that do not exist in `plurx-core/src/cluster/membership.rs`: `capability_ready_predicate` is a SQL fragment evaluated inside Raft transactions, `node_is_tombstoned` a consistent SQL count, and `local_node_is_committed_voter` a Raft metrics read. An agreement test therefore needs either a row-typed read model or a harness that evaluates those SQL predicates over fixtures - a design choice for the session that opens M7, not a move. |
| 2026-09-24 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 | [#511](http://192.168.4.7:3000/noirr/plurx/pulls/511) | Not opened. Each owner migration needs its release-profile shipped-shape test (`cargo test --release -p plurxd <owner>_shipped_shape`), a release build of the plurxd test target this session did not attempt on the shared build host (about 15 GB free, shared with other agents); the first owner (`RollingRetirementSettlement`) is next. |
| 2026-09-25 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | Main merge | [#511](http://192.168.4.7:3000/noirr/plurx/pulls/511) | `e0cb9a2f5` merges `main` @ `8251d14f7` (142 commits past `0e2c3fd47`, among them #505, #513, K-09 and the scratch-grant writes). Main's 80 hunks in the three split parents (`transcode.rs` 52, `http/hls.rs` 7, `vodserve.rs` 21) were re-applied where their code now lives: 76 inside child modules, 4 in the parents (`session_log_text`, the `TranscodeManager` struct, the `vodserve` constants block); 77 matched their base context uniquely in one file, 3 were placed by hand (a context spanning an impl-chunk boundary, a new first `impl VodServe` method, a signature rustfmt had wrapped). Main items that a sibling child now reaches gained `pub(super)` only: 1 in `producer/prepublication_executor.rs`, 1 in `rolling/session.rs`, 8 in `vod/session.rs`, 2 in the `TranscodeManager` impl chunks; the `MOVES` counts carry them. `scripts/split-identity origin/main --moves` (BASE is the new merge base; the script already took BASE as an argument, only its docstring and usage changed) reports `OK` for all three parents, which shows no main change was dropped or duplicated. `plurxd` test list: 2,866 names (2,853 passed, 13 ignored) = main's plus the two M6 `accept_step` tests (source scan of both trees differs by exactly those two). |
| 2026-09-25 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | Review of #511 | [#511](http://192.168.4.7:3000/noirr/plurx/pulls/511) | Answers the single adversarial review ([comment 4695](http://192.168.4.7:3000/noirr/plurx/pulls/511#issuecomment-4695)); disposition in the PR thread. **Main merges:** `9c3ff5332` merges `main` @ `b47c5ff88`: #500's per-method delivery meters conflicted in `transcode.rs` (4 hunks) and `vodserve.rs` (1) and were re-applied in `manager/cache.rs`, `manager/start.rs` (two constructors), `manager/describe.rs` (`test_software_threads_in_use`) and `vod/serve/create.rs`; `63414cfb6` merges `main` @ `38f61dfe6`: #517's store argument to `warm_vtt_window` re-applied in `http/hls/subtitle_playlist.rs`. `scripts/split-identity origin/main --moves` reports `OK` for all three parents against each new merge base. **Findings:** (1) `accept_step_rejection_order` now pins the order with nine pair rows (M6 row); (2) the `accept_step` doc comment names three non-atomic sites, not four; (3) the three hls source scans read `hls_product_source()`, checked against a walk of `http/hls/` on every call, so an unlisted product child fails them (checked with a planted `unlisted_child.rs`); (4) **behaviour restored**: the 299 tracing events the moves put in child modules had been relabelled with the child's module path (`plurxd::transcode::manager_start` and so on); each now names its parent's target, `scripts/split-identity` removes exactly those pins before comparing, and `split_children_log_under_their_parent_target` plus three captured-event tests pin it (all four fail with the pins reverted); guardrail 1 now names the target; (5) the M3 deviation and the post-merge line counts (1,810 / 191 / 751) are recorded in the M3 and M5 rows. |
| 2026-09-25 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M7 | [#543](http://192.168.4.7:3000/noirr/plurx/pulls/543) | Built under **Decision D-M7** (§3.8; Paul can overturn it): row types used only by the projection, and an agreement test that runs the production SQL itself over fixtures. `crates/plurx-core/src/cluster/membership/lifecycle.rs`: `ClusterNodeRow`, `CapabilityRow`, `RemovalRow`, `PromotionRow`, `MaintenanceRow` in `NodeRows`, `RaftMembershipView`, and `observed_lifecycle(NodeRows, &RaftMembershipView) -> LifecycleView` with independent `membership`, `durable_role`, `join`, per-capability readiness, `maintenance`, `removal` (`InProgress{attempts}` / `Tombstoned`) and `promotion` (`Started` / `Barrier{index}` / `Joint`); nothing in the daemon calls it. `cargo test -p plurx-core --features hiqlite-store --lib cluster::membership::tests::lifecycle_projection_agrees` (the crate's cluster module needs the feature; `make unit` gets it by workspace unification) exit 0 in 4.4 s: 2,592 row shapes (role NULL/`learner`/`voter` × removed or not × staged or not × subject capability missing/current/stale × anchor current/stale × no fence or a fence with 0/1/2 attempt references × no/requested/acknowledged maintenance × no promotion / no barrier / barrier) written into `MEMBERSHIP_SCHEMA`'s tables plus the additive columns, read back, and checked against `capability_unready_node_predicate` (both nodes, two capabilities), `capability_ready_predicate`, `NODE_TOMBSTONED_SQL` and `admitted_learner_nodes_sql`, each under five real `openraft::Membership` values (absent, learner, voter, joint entering, joint leaving) checked against `committed_voter` and a hand-written membership table: 12,960 evaluations, every boolean verdict reached both ways. Production reads unchanged: the tombstone count moved verbatim into `NODE_TOMBSTONED_SQL` and `local_node_is_committed_voter` calls `lifecycle::committed_voter` (its old `voter_ids().any(== raft_id)` rule); no SQL text changed. Mutations, each run and each failing the test: a staged node counted as holding back readiness; a pending removal fence not counted as tombstoned; the joint interval collapsed into `Started`/`Barrier`; a Raft learner projected as absent. The transition function stays a later PR, after one release of agreement. |
| 2026-09-25 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 / `RollingRetirementSettlement` | [#543](http://192.168.4.7:3000/noirr/plurx/pulls/543) | First owner migrated. Its `#[cfg(test)] wait_before_await_pause` field, the field's initialiser and the two `#[cfg(test)]` statements in `wait()` became one `hooks: Box<dyn RetirementSettlementHooks>` in every build, with `before_await_settled()` awaited where the pause was. Production installs `NoopRetirementSettlementHooks`, whose future is a zero-sized ready future (boxing it allocates nothing; awaiting it is one poll). **Deviation from §3.9's shape, recorded:** one small trait per owner, named for that owner's points, held as `Box<dyn>` rather than one shared `Hooks` trait behind an `Arc`: the owners' points do not overlap, and the box of the zero-sized no-op does not allocate. Census (`validation/cfg_test_census.py`): the owner's `field` 1 → 0 and `statement` 3 → 0 (whole tree `field` 186 → 185, `statement` 252 → 249); the rest of `rolling/retirement.rs`'s `#[cfg(test)]` lines belong to `spawn_rolling_scratch_cleanup_owner` and other owners. `retirement_settlement_registers_notify_before_the_wait_gap` keeps its barrier pause through a test hook; `rolling_retirement_settlement_shipped_shape` runs the same scenario with the production constructor, polling one waiter by hand (no task, no timer). Debug run of both: exit 0. Mutations, each run: taking the `Notify` interest after the wait gap fails the race test (exit 101); a production hook that never becomes ready fails the shipped-shape test (exit 101). Release profile: `cargo test --release --locked -p plurxd --bin plurxd rolling_retirement_settlement_shipped_shape` on nuc3 exit 0 (release build 7 m 58 s; 1 passed); the release artefacts were deleted afterwards (disk under 20 GB). `tests/playback/rolling-producer-owners.toml`, measured by zeroing each row: `m4-exact-rolling-retirement-settlement` 7 → 8 and `process-lifecycle-method` 399 → 400, both the shipped-shape test's one constructor call and one `wait()`, reviewed in the file. **Not done:** the remaining owners in §5.9's order (`AttemptChild` next, then `Encoding.admission_pause`, `DecodeFactSource`, the `playback_control.rs` seams, the `hls.rs` fault helpers), and §7 Q3 (the `syn` pass that splits the 153 `statement` seams into pauses and one-shot faults) is not answered here; this owner was a pause, so it took the trait. |
| 2026-09-25 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M3 remainder | [#543](http://192.168.4.7:3000/noirr/plurx/pulls/543) | Not attempted. The declarations-only move of `StartInfo`, `probe_media_origin` and the ~30 shared helpers out of `transcode.rs` (M3 row above) is still a follow-up move PR with its own identity recipe. |
| 2026-09-26 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | Review of #543 | [#543](http://192.168.4.7:3000/noirr/plurx/pulls/543) | Answers the single adversarial review ([comment 5261](http://192.168.4.7:3000/noirr/plurx/pulls/543#issuecomment-5261)); disposition in the PR thread. **Main merge:** `acc6a6b1e` merges `main` @ `4b112204b`; the one conflict was `tests/playback/rolling-producer-owners.toml`'s `process-lifecycle-method` row, re-measured by zeroing it: 405 (main's 404 plus this branch's one). **Finding 1:** the M7 row above claimed each dimension was checked; `maintenance` and the removal attempt set were not asserted, and the committed-voter check compared the projection with its own rule. `lifecycle_projection_agrees` now agrees `maintenance` with `NODE_MAINTENANCE_COUNT_SQL` and `EXIT_MAINTENANCE_SQL`, the attempt set with `ROLLBACK_REMOVAL_FENCE_SQL`, `BEGIN_REMOVAL_INTENT_SQL` and `EXISTING_REMOVAL_ATTEMPT_SQL`, promotion existence with `NODE_PROMOTION_COUNT_SQL`, and `membership` with `committed_voter_ids!` (the read `local_node_is_committed_voter` makes); the `Started`/`Barrier`/`Joint` split is recorded as having no production reader (§3.8). Mutations, each run: acknowledged-for-unacknowledged maintenance, an emptied attempt set, both together (the reviewer's pair), and `committed_voter_ids!` reading only the last joint configuration each fail the test. **Finding 2:** §7 Q3 moved to the `AttemptChild` PR and §3.9 records the per-owner `Box<dyn …Hooks>` shape as built. |
| 2026-09-26 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 / `AttemptChild` | [#556](http://192.168.4.7:3000/noirr/plurx/pulls/556) | `f83df478d`. **Premise re-verified:** `transcode/producer/attempt_child.rs` held the three `#[cfg(test)]` pause fields (`:178-185` at base `91f363154`), six `#[cfg(test)]` clones of them (`:261-275`), three gated pause blocks in the supervisor (authorization `:314-322` and flow reservation `:335-345`, both inside the producer transition fence and blocking on a `std::sync::Barrier`; before-reap `:387-395`, an awaited `Notify` pair) and the gated initialisers (`:451-456`); every test construction went through the test-only `AttemptChild::new`, and the pauses were installed after construction (`pause_signal_after_*`, and the `terminate_before_reap_pause` field in three `chunk_06.rs` tests). All three seams are ordering pauses. **Built:** one `Arc<dyn AttemptChildHooks>` held by the child in every build, the supervisor running through a clone taken from it; production `new_with_job` installs `NoopAttemptChildHooks`, the test-only `new` installs `AttemptChildPauses` (reached by `Any` upcasting). The two fence points are synchronous hook methods (no await point there in either build, as before); the before-reap point is an awaited `HookFuture` whose no-op is the zero-sized `HookReady` #543 introduced, now shared. **Deviations, recorded:** `Arc` instead of #543's `Box` (two holders), and two owners in one PR (§5.9 says one per PR; each owner is its own commit). Census (`validation/cfg_test_census.py`, pattern corrected in `f8337377b` to accept `pub(super)`; base re-measured with the corrected script): `attempt_child.rs` `field` 3 → 0, `statement` 10 → 0 (under the old pattern: 2 → 0 and 16 → 7, the 7 being `pub(super) fn`/`struct` lines it mis-filed). Race tests kept through the hooks and green: `producer_signal_and_retirement_share_one_authorization_linearization`, `actor_task_exit_fences_a_reserved_signal_and_cleanup_still_progresses`, `published_failure_cleanup_survives_waiter_cancellation_and_retains_media`, `prepublication_retirement_holds_admissions_until_confirmed_reap`, `first_media_settlement_gap_keeps_confirmed_reap_ownership`. Shipped shape: `attempt_child_shipped_shape` runs `new_with_job` through all three points (suspend and resume publish the held and running flow for the attempt; a termination request reaches the published reap within 5 s). Mutations, each run: the production before-reap hook never ready fails the shipped-shape test (exit 101); the authorization hook moved outside the transition fence made the linearization race test hang instead of fail: its assertion fired, but the supervisor stayed parked on its second unbounded `std::sync::Barrier` wait, so the runtime never dropped (the review measured `timeout 60` exit 124; this row first said exit 101, which was wrong). The review row below bounds the pause, and the same mutation now fails by name. `tests/playback/rolling-producer-owners.toml`, measured by zeroing each row: `supervisor-registration` and `rolling-supervisor-construction` 22 → 23, `namespaced-task-spawn` 639 → 640, `namespaced-time-constructor` 1009 → 1010, all the shipped-shape test's, reviewed in the file. No fleet or device step (§6). |
| 2026-09-26 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | §7 Q3 (`syn` pass) | [#556](http://192.168.4.7:3000/noirr/plurx/pulls/556) | `f8337377b`: `crates/plurxd/src/transcode/tests/seam_census.rs`, run with `cargo test -p plurxd --bin plurxd seam_census -- --nocapture`. Answer recorded in §7 Q3: after this PR, 158 gated statements in shipping functions — 48 pause, 17 fault (7 of them `dv_disk.rs`), 15 override, 34 record, 44 plumbing — plus 97 gated fields and variants, 104 initialisers and 19 arms, over 310 production files (33 test-only files skipped). Two fixture tests pin the classifier (every class, `#[cfg(all(test, ..))]`, and test items ignored) and the test-file resolution (`#[cfg(test)] mod`, `#[path]`, `include!` from a test file); `seam_census_of_the_workspace` asserts the migrated owners (`AttemptChild`, `RollingRetirementSettlement`, `DecodeFactSource`) keep no gated sites. The fixture spells `cfg(TEST)` and swaps it before parsing so the line census does not count fixture text. `m4-exact-rolling-retirement-settlement` 8 → 9 (the owner name as a string in that assertion), measured and reviewed. |
| 2026-09-26 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 / `DecodeFactSource` | [#556](http://192.168.4.7:3000/noirr/plurx/pulls/556) | `bc1c632ee`. **Premise re-verified:** `decode_facts.rs` held `#[cfg(test)] initial_identity_delay` / `final_identity_delay` fields (`:640-643` at base), their gated initialisers (`:656-659`) and `#[cfg(test)]`/`#[cfg(not(test))]` getter twins (`:684-702`) read at the two identity observations (`:3400`, `:3496`), which hand the delay to the blocking syscall owner that sleeps only when it is non-zero. Delays installed by consuming test builders on a source the test holds, so the hook is chosen before use. **Built:** `Arc<dyn DecodeFactSourceHooks>` (the source is `Clone`), production `NoopDecodeFactSourceHooks` (both zero), test `IdentityDelays` installed by `with_identity_delay` / `with_final_identity_delay`, each keeping the delay it does not set; the twins are gone. Census: `decode_facts.rs` `field` 8 → 4 (the owner's two fields and two initialisers; the four left belong to other owners in the file), `statement` 13 unchanged (other owners). Race test `blocked_source_identity_returns_deadline_without_releasing_probe_ownership` kept, and the manager tests that use `with_final_identity_delay` (`source_change_refuses_bound_plan`, `prepared_plan_keeps_producer_execution_out_of_the_probe_lane`) pass in the full suite. Shipped shape: `decode_fact_source_shipped_shape`, the race test's scenario with `DecodeFactSource::new`, returns the fixture probe's `InvalidJson` inside the 100 ms budget with the probe lane free. Mutations, each run (exit 101): a production initial delay of 1 s fails the shipped-shape test; a getter ignoring the hook fails the race test. **Taken ahead of `Encoding.admission_pause`:** that owner's race tests (`vod/tests/chunk_03.rs` ×3, `vodencode_tests.rs`) install the pause after construction on `Encoding`s production code builds (`transcode/manager/create.rs:966`, reached through the VOD recipe fixtures), and on one specific `Encoding` of a predecessor/successor pair. A hook held by an `Encoding` cannot be replaced once it is inside its `Arc`, so the options are a test-build constructor twin (what M8 removes) or a hook factory held by `TranscodeManager` (itself an owner with eight test-only fields) that can target one rendition — a design choice for the `Encoding` PR, not guessed here. |
| 2026-09-26 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | Gates for #556 | [#556](http://192.168.4.7:3000/noirr/plurx/pulls/556) | On nuc3 in `~/work/hc4` at `bc1c632ee` (base `91f363154`; `main` has since gained only #552's one-line K-09 board edit): `cargo fmt --all -- --check` exit 0; `cargo clippy --workspace --all-targets --locked -- -D warnings` exit 0; `cargo test --locked --no-fail-fast -p plurxd` exit 0 (2,957 passed, 14 ignored, compiled from this worktree); `cargo test --release --locked -p plurxd --bin plurxd -- attempt_child_shipped_shape decode_fact_source_shipped_shape rolling_retirement_settlement_shipped_shape` exit 0 (release build 10 m 10 s, 3 passed), release artefacts deleted afterwards (disk under 20 GB); `make history-check` 0, `make validation-lint` 0, validation unittests 0 (246 tests), `make operations-check` 0, `make spike-lock-check` 0. **Remaining M8 owners:** `Encoding.admission_pause` (design question above), the `playback_control.rs` owners, the `http/hls/` sites, the `dv_disk.rs` faults (a `fail`-feature candidate, §7 Q3), and the owners the census lists that §5.9 does not name (`TranscodeManager`, `Session`, `LiveTvManager`, `JobManager`, VOD `Shared`/`Rendition`). |
| 2026-09-26 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | Review of #556 | [#556](http://192.168.4.7:3000/noirr/plurx/pulls/556) | Answers the single adversarial review ([comment 5375](http://192.168.4.7:3000/noirr/plurx/pulls/556#issuecomment-5375)); disposition in the PR thread. **Main merge:** `94e9af103` merges `main` @ `7d545ffce` (#550's Apple test and #552's board line), no conflicts; `cargo test --locked --no-fail-fast -p plurxd` after the merge, before any change: exit 0 (2,957 passed, 14 ignored). **Finding 1 (P2):** `801035dff`. `AttemptChildPauses`' two fence points took a `std::sync::Barrier` and waited on it twice, unbounded; they now take a test-only `SupervisorPause` (a `Mutex`/`Condvar` rendezvous in `producer/attempt_child.rs`). The supervisor side announces the point and waits for release for at most 10 s; the test side (`wait_reached`) panics naming the point if the supervisor does not arrive within 10 s, and returns a `SupervisorPauseHeld` guard whose `release` or drop lets the supervisor go, so a failed assertion releases it while the test unwinds. Both race tests use the guard. Pinned by `supervisor_pause_releases_its_supervisor_when_the_test_unwinds` (a panic inside the hold releases a supervisor thread promptly, not at its 30 s bound), `supervisor_pause_names_a_point_the_supervisor_never_reaches` and `supervisor_pause_lets_the_supervisor_go_when_the_test_never_arrives`. Mutations, each run on a binary copied out of the shared target (`timeout 60`): the reviewer's (authorization hook above `lock_producer_transition()`) — before this change exit 124 after 60 s, after it exit 101 in 0 s at `the paused supervisor must own the transition guard`; the flow-reservation hook above the fence — exit 101 in 0 s at `the reserved signal owns the transition fence before its syscall`; the guard's `Drop` removed — the unwind test fails (exit 101, 30 s). `DecodeFactSource`'s hooks are `Duration`s the identity owner sleeps for, never a rendezvous, so they cannot hang this way; the before-reap point is an awaited `Notify` pair the runtime drop cancels. `tests/playback/rolling-producer-owners.toml`, measured by zeroing each row: `namespaced-task-spawn` 640 → 641 (the unwind test's supervisor thread) and `process-lifecycle-method` 405 → 397 (the eight `Barrier::wait` calls removed), reviewed in the file. Gates at `801035dff` plus this row: `cargo fmt --check` 0, `cargo clippy --workspace --all-targets --locked -D warnings` 0, `cargo test --locked --no-fail-fast -p plurxd` 0 (2,960 passed, 14 ignored; compiled from this worktree), `make history-check` 0, `make validation-lint` 0, validation unittests 0 (246), `make operations-check` 0, `make spike-lock-check` 0. The release-profile shipped-shape run was not repeated: the change is test-only and the production `NoopAttemptChildHooks` it exercises is unchanged. |
| 2026-09-26 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 / `Encoding`, `RollingFlowSync`, `TerminalCleanup`, `HeadChildOwner`, `VodServingAdmission` | [#562](http://192.168.4.7:3000/noirr/plurx/pulls/562) | `9e6e1e8d4`, branched from `main` @ `2ab0cd497` (#556 merged). **Premises re-verified at `2ab0cd497`:** `vodencode.rs` `#[cfg(test)] pub admission_pause: Mutex<Option<Arc<Barrier>>>` (`:48-49`), its initialiser in the production recipe (`transcode/manager/create.rs:965-966`) and the test clone (`vodencode.rs:114`), and the gated take-and-wait-twice in `try_permit` (`:141-151`); `playback_control.rs` `RollingFlowSync::wait_after_check` (`:6785-6786`, `:6796-6797`, the pause in `wait_for` `:6833-6843`); `vod/session.rs` `TerminalCleanup::wait_enabled_pause` (`:249-250`, `:259-260`, the block in `wait` `:293-303`) and `HeadChildOwner::reap_pause` (`:328-329`, both production constructors `:338-339`/`:351-352`, the test constructor `:356-367`, the reaper's wait-twice `:374-383`); `vodserve.rs` `VodServingAdmission::pause_before_commit` (`:502-503`, `:521-522`, builder `:526-530`, the wait-twice in `commit_guard_before` `:533-537`). All five are ordering pauses on a `tokio::sync::Barrier`. **Premise corrected (the #556 row):** no test pauses an `Encoding` that production code built. Every pausing test builds its own: `vodencode_tests.rs`'s `encoded_fixture` literal (`:138`), the test's own `fresh` literal (`:351`), and `Encoding::clone_with_admissions_for_test` (`vodencode.rs:87`), which `vod/tests/chunk_03.rs`'s `encoded_driver_fixture` (`:1274-1277`) and `prepared_handoff_fixture` (`:1722-1726`) use for both halves of the predecessor/successor pair. The four pausing sites (`vodencode_tests.rs:575`, `chunk_03.rs:1509`, `:1932`, `:2138`) arm an encoding the test built. **Built:** per-owner hook traits, the race tests kept through `seam_hooks::AsyncPause` (§3.9 as built), and one shipped-shape test per owner through its production constructor: `encoding_shipped_shape` (`prepare_vod_encoding`, then `try_permit` admits inside 5 s), `rolling_flow_sync_shipped_shape` and `terminal_cleanup_shipped_shape` (the race scenario polled by hand, no task and no timer), `head_child_owner_shipped_shape` (`HeadChildOwner::new` reaps a real child inside 5 s) and `vod_serving_admission_shipped_shape` (commits for the current generation, refuses after serving is lost). Five `seam_hooks` tests pin `AsyncPause`: hold until release, release on unwind, a named panic for a point never reached (and the disarm), the owner's own bound, and the wall-clock bound under a paused runtime. **Census:** `validation/cfg_test_census.py` whole tree `field` 197 → 186, `statement` 163 → 155. Each owner's own lines (fields and initialisers, then statements) went to 0: `Encoding` 2 and 2, `RollingFlowSync` 2 and 2, `TerminalCleanup` 2 and 1, `HeadChildOwner` 3 and 2, `VodServingAdmission` 2 and 1. The remaining `#[cfg(test)]` lines in those files belong to other owners (`vod/session.rs`'s two are `Rendition`'s `stopped_poll_*`). The `syn` census (`seam_census_of_the_workspace`, which now also asserts these five keep no gated site): 312 production files (`seam_hooks.rs` among them), gated statements `pause` 48 → 43 and `plumbing` 44 → 41, `field` 97 → 92, `initialiser` 104 → 98; `fault` 17, `override` 15, `record` 34 and `arm` 19 unchanged (the before figures measured in this worktree at #556's `c7ae7b15f`, not at `2ab0cd497`). **Mutations, each run on a copy of the test binary (`timeout 90`):** (a) every production no-op hook returns `std::future::pending()`: all five shipped-shape tests fail (exit 101; `encoding_shipped_shape`, `head_child_owner_shipped_shape` and `vod_serving_admission_shipped_shape` at their 5 s bound, the two hand-polled ones at their second poll). (b) The defect each race test pins: registering the `Notify` interest after the hook point instead of before the check fails `flow_completion_between_check_and_await_cannot_be_lost` (exit 101). *Not true as first written for `TerminalCleanup`:* at `9e6e1e8d4` its hook sat before the re-check, so a completion at the hook was seen by that re-check and `terminal_cleanup_completion_after_wait_registration_is_not_lost` passed with registration moved after the re-check (reproduced by the review, exit 0). The hook now sits after the re-check; see the "Review of #562" row. `terminate_and_reap` not awaiting its reaper fails `cancelled_build_waiter_cannot_release_same_key_before_confirmed_head_reap` (exit 101, "same-key retry cannot acquire spawn authority while reap is paused"). `commit_guard_before` handing out a guard without the generation check fails `serving_loss_racing_final_cluster_attachment_leaves_no_session` (exit 101). The resume line at the ahead horizon (`prodsched.rs`, no hysteresis) fails `a_yielded_encoder_does_not_take_the_permit_back_while_still_ahead` at `the rendition must not re-admit above the low-water line` (exit 101). **Limit found, not changed:** deleting only `notified.as_mut().enable()` fails neither `Notify` race test, before this PR or after it: Tokio's `notify_waiters` also wakes a `Notified` future that has only been constructed, so the tests pin the order of registration and check (`TerminalCleanup`'s only since the "Review of #562" row), not the `enable` call. **`tests/playback/rolling-producer-owners.toml`**, measured by zeroing each row (reviews in the file): `m4-serving-authority-projection` 34 → 35, `m4-serving-authority-commit-guard` 15 → 17, `m4-vod-terminal-compaction-owner` 20 → 22, `namespaced-task-spawn` 643 → 647, `method-spawn` 47 → 48, `namespaced-time-constructor` 1013 → 1021, `process-command-construction` 188 → 189, `process-capable-launch-method` 389 → 390, `process-lifecycle-method` 397 → 373 (net -24: 25 `Barrier::wait` calls removed and one `cleanup.wait()` added in `terminal_cleanup_shipped_shape`), `free-or-ufcs-process-lifecycle` 50 → 51, `kill-on-drop-construction` 89 → 90; no production task, timer or process was added. **Deviation, recorded:** five owners in one PR (§5.9 says one per PR); each is small, has its own shipped-shape test and mutation, and shares the new `AsyncPause`. No fleet or device step (§6). |
| 2026-09-26 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 decisions | [#562](http://192.168.4.7:3000/noirr/plurx/pulls/562) | **Decision D-M8-E (Paul can overturn it): `Encoding`'s hooks are chosen where the encoding is built; no hook factory on `TranscodeManager`.** The session brief preferred a factory held by `TranscodeManager` because the #556 row said the tests pause encodings that production code builds. Re-verified above, that premise is wrong. Every pausing test builds its encodings itself, and each encoding carries its own `EncodingAdmissionPause`, so a test arms exactly the predecessor or the successor. The production recipe installs `NoopEncodingHooks`. A factory would add a field to `TranscodeManager` (an unmigrated owner with eight test-only fields) that no test uses. **To overturn:** if a test must pause an encoding that `prepare_vod_encoding` builds, `TranscodeManager` gains an `Arc<dyn EncodingHooksFactory>` (production: a no-op factory), `prepare_vod_encoding` asks it for each encoding's hooks, and the test reaches the pause through the encoding's `Any` downcast as now. **Decision D-M8-F (Paul can overturn it): no fault-injection cargo feature for `dv_disk.rs`** (reasons in §7 Q3). **To overturn:** add `dv-disk-fault-injection` to `plurxd`'s features, replace its seven fault sites' `cfg(test)` with `cfg(any(test, feature = …))` or `fail_point!`, and pass `--features` in every `plurxd` test and clippy lane. |
| 2026-09-26 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | Gates for [#562](http://192.168.4.7:3000/noirr/plurx/pulls/562) | [#562](http://192.168.4.7:3000/noirr/plurx/pulls/562) | On nuc3 in `~/work/hc11` at `9e6e1e8d4` (base `2ab0cd497`), every `*.rs` touched and `Compiling plurxd (/home/pjunod/work/hc11/…)` confirmed: `cargo fmt --all -- --check` exit 0; `cargo clippy --workspace --all-targets --locked -- -D warnings` exit 0; `cargo test --locked --no-fail-fast -p plurxd` exit 0 (2,972 passed, 14 ignored; the new tests are in its output); `cargo test --release --locked -p plurxd --bin plurxd -- encoding_shipped_shape rolling_flow_sync_shipped_shape terminal_cleanup_shipped_shape head_child_owner_shipped_shape vod_serving_admission_shipped_shape attempt_child_shipped_shape decode_fact_source_shipped_shape rolling_retirement_settlement_shipped_shape` exit 0 (release build 10 m 42 s, 8 passed), release artefacts deleted afterwards (disk at 18 GB free); `make history-check` 0, `make validation-lint` 0, validation unittests 0 (247 tests), `make operations-check` 0, `make spike-lock-check` 0. **Remaining M8 owners:** the `playback_control.rs` group, which is one connected owner. `RollingControlHandle`'s five gated fields (`producer_attempt_reply_pause`, two abort handles, two `Notify`s) are plumbed through `RollingActorRuntime`, `RollingControlActor`, `RollingDecisionTransport` and `RollingActorExitFence`, beside `RollingProducerIngress`'s wait-started notification and the gated command variants. It is its own PR. After it: `http/hls/`'s sites (function-level pauses in `control.rs`, `create.rs`, `preparation.rs`, `release.rs`, and `StartedSessionCleanup`, whose test hook also ends cleanup early and is therefore an override, not a pause); `TranscodeManager` (8 fields, among them `vod_publication_admission_pause`); `Session`; the VOD `Shared` (7) and `Rendition`; and the census's other owners (`JobManager`, `LiveTvManager`, `SharedCacheCoordinator`, `ArtworkCoordinator`, `OfflineManager`, `MediaSessionCoordinator`). |
| 2026-09-26 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | Review of #562 | [#562](http://192.168.4.7:3000/noirr/plurx/pulls/562) | Answers the single adversarial review ([comment 5510](http://192.168.4.7:3000/noirr/plurx/pulls/562#issuecomment-5510)). `main` @ `d4fa763c7` merged first (`837ad1ee2`): conflicts in the S-14 workboard row (both records kept), `validation/points.toml`'s plurxd media-path list (`fontenv`, `fontenv_tests` and `seam_hooks` all kept) and seven `tests/playback/rolling-producer-owners.toml` rows, each re-measured by zeroing it (`namespaced-task-spawn` 654, `method-spawn` 48, `namespaced-time-constructor` 1037, `process-command-construction` 194, `process-capable-launch-method` 391, `process-lifecycle-method` 376, `free-or-ufcs-process-lifecycle` 53: main's value plus this branch's delta in every row). **Finding 1 (P2), accepted:** `TerminalCleanup::wait` awaited its hook between `enable()` and the finished re-check, so the race test's completion always landed before a state read and the test could not tell a waiter registered before the re-check from one registered after it. The hook is now `CleanupWaitHooks::after_wait_check`, awaited after the re-check and before `notified.await`, the shape `RollingFlowSync::wait_for` has. The production hook is the ready no-op, so production order and behaviour are unchanged. The comment above the registration now states Tokio's rule (a `Notified` counts as registered once `notified()` returns; `notify_waiters` stores no permit). **Mutation, proved on nuc3:** `wait` re-checking, awaiting the hook, and only then calling `notified()`/`enable()` fails `terminal_cleanup_completion_after_wait_registration_is_not_lost` (exit 101, `registered terminal waiter must observe completion: Elapsed(())`); the unmutated binary passes it (exit 0). Both binaries were built from `~/work/hc11` and copied. The earlier M8 row's mutation (b) and `enable` limit were corrected in place, as was its `process-lifecycle-method` note (25 removed and 1 added, net -24, per the review). |
| 2026-09-27 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 / `playback_control.rs` control group | [#573](http://192.168.4.7:3000/noirr/plurx/pulls/573) | `ee8a2bb23`, branched from `main` @ `c61bb6409` (#562 merged; against #562's head `795a73d71`, `main` adds only #558, which touches none of these files). **Premises re-verified at `795a73d71`:** `RollingControlHandle`'s five gated fields (`playback_control.rs:7644-7652`: `producer_attempt_reply_pause`, `actor_abort`, `executor_abort`, `actor_run_started`, `actor_exit_fence_started`), plumbed through `RollingActorRuntime` (`:8338-8342`), `RollingControlActor` (`:8722-8726`, initialisers `:8774-8778` and `:8833-8837`), the exit fence (`:8356`, notified in its `Drop` `:8362`), `run` (`:12279-12282`), `spawn_unbound` and `spawn` (`:12510-12574`); `RollingDecisionTransport`'s two executor pauses (`:8031-8033`, each taken and waited twice at `:8154-8177`); `RollingProducerIngress`'s wait-started notification (`:6893`, notified at `:7308` after the capacity re-check) and `RollingProducerIngressState::last_flow_applied` (`:6923`, recorded at `:7352`); and the deferred `BeginProducerAttempt` reply in `handle_command` (`:11731`, `:11799-11813`, `:12170-12211`), with three `#[cfg(not(test))]` twins. Every test that arms one of these builds its handle through a production constructor (`RollingControlHandle::spawn` directly, or `spawn_prepublication_*` and `spawn` through `transcode/test_support.rs` and the `transcode/tests` fixtures) and arms it afterwards. **Built:** one `Arc<dyn RollingControlHooks>` (nine points: `actor_spawned`, `executor_spawned`, `actor_run_started`, `actor_exit_fence_started`, `after_command_settled`, `before_executor_poll`, `after_executor_poll`, `flow_capacity_wait_started`, `flow_barrier_applied`), held by the transport, runtime, actor, exit fence and ingress in every build. The actor awaits `after_command_settled` at the end of every command, after the transition is released and the executor woken, where the test build awaited the deferred reply; the twins are gone. The test-only `BeginProducerAttempt` arm hands its reply to the test hooks, which deliver it at that point, holding at the armed reply pause, so test behaviour is unchanged. Race tests kept, moved from `Barrier` pairs to `seam_hooks::AsyncPause` (nine tests). Shipped shape: `rolling_control_group_shipped_shape` (production `spawn`: an attempt, a committed decision the executor observes through both poll points, the terminal, and the exit fence on drop, each bounded) and `rolling_producer_ingress_shipped_shape` (a full ingress parks a flow waiter after its re-check and a released reservation wakes it; a flow barrier still enters; polled by hand). **Census** (`validation/cfg_test_census.py`, `main` → head): whole tree `field` 187 → 162, `statement` 160 → 135; `playback_control.rs` `field` 33 → 10, `statement` 24 → 0. The 10 are the six test-driver command variants and four `metric_index` arms (D-M8-H). The `syn` census (`seam_census_of_the_workspace`, which now asserts the group keeps no gated site except the actor's six test-command arms; run against `main`'s tree it fails listing the group's sites, exit 101): `pause` 43 → 39, `override` 16 → 14, `record` 35 → 32, `plumbing` 45 → 31, `field` 92 → 75, `initialiser` 98 → 81 (both commits; `fault` 17 and `arm` 19 unchanged). **Mutations, each on a copy of the test binary (`timeout 90`), built from this commit's pre-rebase twin `7b8b33ac9` (same code):** (a) `NoopRollingControlHooks::after_command_settled` never ready: `rolling_control_group_shipped_shape` fails at its 5 s bound on the second command (exit 101); (b) `before_executor_poll` never ready: the same test fails, `executor never reached "idle"/Some(17); last="executing"/0` (exit 101); (c) `after_executor_poll` moved above the poll it follows: `stale_poll_result_cannot_overwrite_terminal_executor_state` fails, `last="terminal"/0` (exit 101). (f) Added in the #573 review round (finding 1): `after_command_settled` moved above `wake_executor_after_transition` in production `handle_command`. The review showed that this move passed every test, since (a) and (b) pin only that the no-op points are ready. `actor_wakes_the_executor_before_its_command_settles` now holds the actor at the point after a decision install and requires the executor to be woken and observing while it is held. Under (f) it fails, `executor never reached "executing"/None; last="idle"/0` (exit 101). **Not pinned by a mutation:** the synchronous record points (`actor_spawned`, `executor_spawned`, `actor_run_started`, `actor_exit_fence_started`, `flow_capacity_wait_started`, `flow_barrier_applied`); their no-ops do nothing, so no production-shape test can fail on them, and `flow_capacity_wait_started` stays where its notification was, after the re-check. Also not pinned: `before_executor_poll`'s position. (b) pins only that it is ready, and no mutation that moves it was run. No fleet or device step (§6). |
| 2026-09-27 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 / `StartedSessionCleanup` | [#573](http://192.168.4.7:3000/noirr/plurx/pulls/573) | `b75935b7d`. **Premise re-verified at `795a73d71`:** `http/hls/session_guard.rs` `StartedSessionCleanup::test_settlement` (`:216-221`, three oneshots), its initialiser (`:361`), `hold_cleanup_for_test` (`:371`) and the gated block in `StartedSessionGuard`'s `Drop` (`:430-437`). **Premise corrected (the #562 gates row):** the block is not an override. It signalled settlement, waited for the test, dropped the replacement gate, reported the drop and returned; the `return` only skips the final `drop(_replacement)`, whose value that branch had already moved, so no cleanup step is skipped or substituted. The `syn` census already classed it as a pause. **Built:** `Box<dyn StartedSessionCleanupHooks>` in every build: `after_cleanup_settled` awaited after the worker abort and the request-claim settlement, with the gate still held, and `after_replacement_released` after the gate drops. Production installs `NoopStartedSessionCleanupHooks`; `hold_cleanup_for_test` replaces the hooks on the armed guard with a bounded `AsyncPause` and a `released` oneshot. Census: `session_guard.rs` `field` 2 → 0, `statement` 1 → 0; `seam_census_of_the_workspace` also asserts `StartedSessionCleanup` and `StartedSessionGuard` keep no gated site. Race test `started_session_guard_holds_replacement_gate_until_cleanup_settles` kept through the pause; shipped shape `started_session_cleanup_shipped_shape` drops an armed guard built by `StartedSessionGuard::new` and reacquires the player's gate inside the race test's one-second bound. **Mutations (`timeout 90`, pre-rebase twin `867bd67dc`):** (d) the production settlement point never ready fails the shipped-shape test at its 1 s bound (exit 101). *As first written* the shipped-shape test allowed a 5 s deadline and passed under (d): a takeover reclaims an abandoned gate on its own, so the bound was tightened to the race test's. (e) The settlement point moved after the gate's release fails the race test (`replacement waiter registered behind the cleanup-owned gate: RecvError`, exit 101). (e) pins only the upper side of the point. The #573 review (finding 2) showed that hoisting it above the abort and the claim settlement passed both tests, because the race test never looked at the worker or the claim while it held the pause. In the review round, the race test publishes the worker as the guard's own incarnation at owner epoch 1 and claims the guard's request. While the cleanup is held, it asserts that the worker is gone and that the claim can be retried. (g) With the point hoisted to the first statement of the spawned cleanup, the race test fails with `the cleanup aborts the worker before it settles` (exit 101). (h) With the point moved between the abort and the claim settlement, it fails with `the cleanup settles the request claim before it settles` (`InFlight { .. }`, exit 101). |
| 2026-09-27 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 decisions | [#573](http://192.168.4.7:3000/noirr/plurx/pulls/573) | **Decision D-M8-G (Paul can overturn it): the control group's hooks are chosen at construction, and test code calls `_for_test` constructors.** The hooks are held by the actor and executor tasks that `spawn_unbound` starts, so they cannot be swapped after construction, and every arming test builds its handle through a constructor. `spawn_for_test`, `spawn_prepublication_producer_for_test` and `spawn_prepublication_transcode_for_test` install `RollingControlTestHooks`; all test call sites use them (47 `::spawn_for_test(` and the fixtures' prepublication constructors in `playback_control.rs`'s tests, `transcode/test_support.rs` and `transcode/tests/chunk_0{1,2,6,8}.rs`); the production call sites (`manager/start.rs` ×2, `manager/cache.rs`) are unchanged. The test hooks behave as the no-op except when armed, and for the test-only `BeginProducerAttempt` reply, which they deliver after the executor wake as the test build did before. `tests/playback/rolling-producer-owners.toml`'s `namespaced-task-spawn` pattern now also matches `::spawn_for_test(`, so the renamed sites stay counted (without it the row falls to 607 while the tasks stay). **To overturn:** a factory held by the caller that builds the handle, which the test fixtures would still have to reach. **Decision D-M8-H (Paul can overturn it): the six test-driver commands stay `#[cfg(test)]`** (`Renew`, `BeginProducerAttempt`, `MarkStartupPresented`, `ReservePreparationCommit`, `InstallProducerDecision`, `SetRenewalForTest`, their six `handle_command` arms and four `metric_index` arms). They are the test API's messages (§3.9: test helpers stay gated), no production sender builds them, and compiling them into production would ship variants nothing sends. Their arms add no statement to any production arm. The enum's variant set therefore still differs between builds; its size was not measured. **To overturn:** move each test driver onto a production command plus a hook, or ship the variants. **`tests/playback/rolling-producer-owners.toml`**, measured by zeroing each row (reviews in the file): `m4-decision-transport` 6 → 7 (the census assertion's string), `namespaced-task-spawn` 653 → 654 (the alias above, plus the shipped-shape test's production `spawn`), `namespaced-time-constructor` 1044 → 1049 (five test-side timeouts in the two shipped-shape tests), `process-lifecycle-method` 376 → 352 (24 `Barrier::wait` calls removed). |
| 2026-09-27 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 remaining owners, re-verified | [#573](http://192.168.4.7:3000/noirr/plurx/pulls/573) | Not migrated here; premises read at `c61bb6409` for the next PRs. **`Rendition`** (`vod/session.rs:90-93`): two `Notify` records, `stopped_poll_armed` and `stopped_poll_fired`, set in the driver (`vod/driver.rs:24-35`) and awaited by three `vod/tests/chunk_03.rs` tests. Tests build their renditions (`synthetic_rendition`, `vod/tests/chunk_01.rs:790`; `vod/serve/construct.rs:172`) and production builds one (`vod/shared.rs:714`), so the per-owner `Box` shape with the hooks chosen at the literal fits without a factory; the next owner. **`TranscodeManager`** (`transcode.rs:1332-1354`, `:1525-1530`): eight fields, mixed: two pauses (`subtitle_playlist_commit_pause`, `vod_publication_admission_pause`), one delay handed to `DecodeFactSource` (`decode_source_final_identity_delay`), two overrides (`force_artifact_qualification`, and `offline_produce_script`, which replaces `produce_normalized`), two records (`manifests_published`, `offline_produced_recipes`) and one fault (`fail_next_offline_recovery_begin`). It is built by `TranscodeManager::new` at about 140 test sites and 20 `state.rs` sites and armed after construction, so it needs D-M8-G's construction-time choice across those fixtures; its own PR. **`Session`** (`transcode/rolling/session.rs:78-139`): 14 fields (13 pauses, `retirement_started` a record), built by `manager/start.rs` ×2, `manager/cache.rs` and `test_support.rs`; its own PR. **VOD `Shared`** (`vodserve.rs:687-705`): six `Barrier` pauses and `terminal_route_test_outcomes`, an override. **The other `http/hls/` sites** (`syn` census at this head): `control.rs` `control_local_with_settlement_capacity` (1 pause, 1 fault) and `settle_preparation_control` (2 pauses); `create.rs` `create_with_purpose` (1 pause); `preparation.rs` `process_preparation_candidate` (1 pause, 1 record) and `stage_prepared_successor_with_prime` (1 pause), plus the `#[cfg(not(test))]` twin at `:1292-1313` (tests stage without priming: an alternate implementation); `release.rs` `release_session` (2 pauses) and `end_media_session_for_release` (1 fault); `status.rs` and `subtitle_playlist.rs` (1 record each). All are free functions fed by module statics keyed by session, incarnation or playback id (`release_pauses`, `CREATE_ASK_RECORDED_PAUSE`, `preparation_settlement_delays` and the rest); none has a gated field or initialiser, so there is no struct to hold a hook. Unlike `dv_disk.rs`'s faults (D-M8-F), most are pauses in async functions, whose await points do differ between builds. Moving them needs an owner every caller reaches, most plausibly `AppState` holding an `Arc<dyn HlsRouteHooks>` with the keyed statics moved into its test implementation. That is a design choice for Paul or the next session, not made here. |
| 2026-09-27 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | Gates | [#573](http://192.168.4.7:3000/noirr/plurx/pulls/573) | On nuc3 in `~/work/hc6`, every `*.rs` touched and `Compiling plurxd (/home/pjunod/work/hc6/…)` confirmed. At `b75935b7d` (base `c61bb6409`): `cargo fmt --all -- --check` exit 0; `cargo clippy --workspace --all-targets --locked -- -D warnings` exit 0; `cargo test --locked --no-fail-fast -p plurxd` exit 0 (3,007 passed, 15 ignored; the three new shipped-shape tests and `seam_census_of_the_workspace` are in its output); `cargo test --release --locked -p plurxd --bin plurxd -- rolling_control_group_shipped_shape rolling_producer_ingress_shipped_shape started_session_cleanup_shipped_shape` and the eight earlier owners' shipped-shape tests exit 0 (release build 17 m 06 s, 11 passed), release artefacts deleted afterwards (disk at 30 GB free). With the ledger and this log (`a6316608c`): `make history-check` 0, `make validation-lint` 0, validation unittests 0 (247 tests), `make operations-check` 0, `make spike-lock-check` 0. No web, Android or Apple file changed, so those lanes were not run. |
| 2026-09-27 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 / `Rendition` | [#579](http://192.168.4.7:3000/noirr/plurx/pulls/579) | `bd85bfccf`, branched from `main` @ `319569b6a` (#573 and #578 merged). **Premise re-verified at `319569b6a`:** `vod/session.rs:88-93` held the two gated `Notify` records (`stopped_poll_armed`, `stopped_poll_fired`), notified by the gated statements in `vod/driver.rs:24-25` and `:32-35`, initialised at three struct literals: production `build_rendition` (`vod/shared.rs:742-745`), the test-only producer-less HTTP fixture (`vod/serve/construct.rs:171-174`) and the test-only `synthetic_rendition` (`vod/tests/chunk_01.rs:828-831`); three `vod/tests/chunk_03.rs` tests awaited them (`:1424`/`:1432`, `:1474`/`:1476`, `:1577`/`:1581`). **Built:** `Box<dyn RenditionHooks>` in every build, chosen at the literal: production and the HTTP fixture install `NoopRenditionHooks`, `synthetic_rendition` installs `RenditionTestHooks` (the two `Notify`s, reached through `RenditionTestHooks::of`). Both points are synchronous records; the driver calls them where it notified. **Shipped shape:** `rendition_shipped_shape` builds an encoded rendition through the production `build_rendition`, asserts it carries `NoopRenditionHooks`, and drives the real stopped-encoder poll to its yield with no observation point, moving the paused clock one poll at a time (the horizon set-up moved into `park_stopped_at_horizon`, shared with the fixture). **Census:** `validation/cfg_test_census.py` whole tree `field` 164 → 156, `statement` 142 → 140. **Mutation (`timeout 90`, on a copy of the test binary):** `stopped_poll_armed()` moved after the poll's wait fails `a_rolling_live_start_also_releases_a_stopped_encoder` (`a rolling-live waiter sends no VOD registry kick`, exit 101); the other two stopped-encoder tests pass under it (exit 0). The points are records whose no-op does nothing, so no production-shape test can fail on them. |
| 2026-09-27 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 / VOD `Shared` | [#579](http://192.168.4.7:3000/noirr/plurx/pulls/579) | `a49121d7e` and the position tests `9f438b7cb`. **Premise re-verified at `319569b6a`:** `vodserve.rs:685-705` held six `Barrier` pauses (`terminal_replay_pause`, `control_applied_pause`, `segment_ready_pause`, `terminal_detach_pause`, `rendition_install_pause`, `dormant_purge_pause`) and the `terminal_route_test_outcomes` override, initialised once in `new_configured` (`vod/serve/construct.rs:342-355`), which every production and test constructor reaches; the sites were `vod/serve/control.rs:113-125` and `:383-395`, `vod/serve/serve.rs:507-519` and `:524-535`, `vod/serve/end.rs:19-29` and `vod/shared.rs:90-105`, `:432-444`, `:896-906`; `transcode/manager/maintenance.rs:495-498` forwarded the detach pause to `http/hls/tests/chunk_01.rs:1247`. **Premise added:** `segment_ready_pause` was one barrier waited in two places in three phases (after the post-registration re-check, then twice in the `Ready` arm), so it becomes two points. Every arming test builds the registry through a production constructor (`VodServe::new_cluster` via `local_serve`/`bare_serve`, or `TranscodeManager::new`) and arms it afterwards through `serve.shared`, so the hooks cannot be chosen at construction without a construction-time choice threaded through `VodServe`'s three constructors and `TranscodeManager::new` (Decision D-M8-I). **Built:** `seam_hooks::HookSlot<dyn VodSharedHooks>` in every build: seven awaited points (`before_terminal_replay_join`, `after_control_applied`, `after_segment_wait_registered`, `before_segment_ready_open`, `before_terminal_detach`, `after_rendition_installed`, `after_dormant_purge_removed`) where the barriers were, and `terminal_route_outcome`, the override as a return value (`None` in production, so the Store is read). `VodSharedTestHooks` holds seven `seam_hooks::PauseSlot`s (an armable one-shot `AsyncPause`) and the route table. Race tests kept, moved from `Barrier` pairs to `AsyncPause` (six in `vod/tests`, one in `http/hls/tests`). **Shipped shape:** `vod_shared_shipped_shape` builds the registry through the production constructor, asserts the slot reads `NoopVodSharedHooks`, polls all seven no-op points ready at their first poll, reads a route through the Store, and serves a blocked GET across both segment points. **Census:** `field` 156 → 142, `statement` 140 → 126; `seam_census_of_the_workspace` now also asserts `Shared`, `Rendition`, `spawn_driver` and the three `VodServe` sites keep no gated site; the `syn` census (`seam_census_of_the_workspace`, 352 production files) `pause` 39 → 32, `override` 16 → 15, `record` 32 → 30, `plumbing` 36 → 30, `field` 76 → 67, `initialiser` 82 → 73 (`fault` 17 and `arm` 19 unchanged; `main` figures measured in this worktree at `319569b6a`). **Mutations (`timeout 90`, each on a copy of the test binary; exit 101 = the test failed):** (a) every no-op point `pending()`: `vod_shared_shipped_shape` fails (101). Install point hoisted above the working-set accounting: `cancelled_real_attach_is_accounted_reusable_and_purgeable` fails (101). *Survived as first written:* the install point moved after the admission check passed every test (exit 0); `rendition_install_point_precedes_admission` (a fully adopted rendition, not admitted while held) now fails under it (101). Dormant point moved into the caller before the settlement spawn: `cancelled_dormant_purge_keeps_key_and_accounting_owned_until_settlement` fails (101). *Survived as first written:* the dormant point moved after the producer's termination passed (0); `dormant_purge_point_precedes_producer_termination` now fails under it (101). Registration point moved after the wait: the blocked-GET test fails (101). Ready point moved before the wait: fails on its new assertion (101). *Survived as first written:* the ready point moved after the file open passed (0); `segment_ready_point_precedes_the_file_open` (the file unlinked while held) now fails under it (101). Applied point hoisted above the commit: `cancelling_after_control_acceptance_keeps_the_anchor_and_exact_replay` fails (101). *Survived as first written:* the applied point moved after the live path's marker-prewarm step passed (0); the VOD End test now holds the point and fails under it (101, the End path no longer reaches it). Replay point moved after `cleanup.wait()`: the End test fails (101). Replay point hoisted above the lookup, to just after the lifecycle gate: the accepted-control test, which now keeps the replay point armed through a live exchange and a live replay, fails (101). Detach point moved after the reader detach: the End test and `durable_delete_tombstone_blocks_media_before_local_stop` fail (101 each). |
| 2026-09-27 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 decisions | [#579](http://192.168.4.7:3000/noirr/plurx/pulls/579) | **Decision D-M8-I (Paul can overturn it): owners that tests arm after construction, through the `Arc` their tasks share, hold a `seam_hooks::HookSlot`.** The slot is a set-once `OnceLock<Box<dyn …Hooks>>` beside a `&'static` no-op, in every build; production never fills it, so every point reads the no-op (one atomic load per point); a test fills it with the owner's test hooks the first time it arms a point. Used here for the VOD `Shared`; `Session` (armed through the manager's registry after `manager/start.rs` builds it) and `TranscodeManager` (built at about 160 sites, armed after construction) take it next, so neither PR has to change its construction sites. **To overturn:** choose the hooks at construction (D-M8-G's shape) with `_for_test` constructors threaded through `VodServe::new*` and `TranscodeManager::new`, which every fixture would have to call. **Decision D-M8-J (Paul can overturn it): the remaining `http/hls/` sites move to one `HlsRouteHooks` trait held by `AppState` in a `HookSlot`,** one trait for the route group as D-M8-G did for the control group, with the keyed module statics (`release_pauses`, `CREATE_ASK_RECORDED_PAUSE`, `preparation_settlement_delays` and the rest) moved into its test implementation as keyed tables. Premise checked at `319569b6a`: every function that holds a site takes `AppState` (`control.rs:523` `settle_preparation_control` and `:2261` `control_local_with_settlement_capacity`, `create.rs:1239` `create_with_purpose`, `preparation.rs:1117` `process_preparation_candidate` and `:1451` `stage_prepared_successor_with_prime`, `release.rs:161` `release_session` and `:347` `end_media_session_for_release`). Not built here. **To overturn:** a hook parameter per function, or one `HookSlot` static per module. **Scope (a deviation, recorded):** two owners in one PR, as #562 and #573 did, each its own commit and each proved separately. `Session` (14 fields, 13 of them awaited pauses, each needing its position pinned) and `TranscodeManager` (two overrides and a fault as well as pauses) are each their own PR. |
| 2026-09-27 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 remaining owners, re-verified | [#579](http://192.168.4.7:3000/noirr/plurx/pulls/579) | Premises read at `319569b6a` for the next PRs. **`Session`** (`transcode/rolling/session.rs:78-139`): 14 fields, 13 `Barrier`/`LifecycleTestPause` pauses and `retirement_started`, a record; sites in `session.rs` (`:1229`, `:1255`, `:1276`, `:1662`, `:1728`, `:1813`, `:1921`, `:2080`, `:2313`), `rolling/retirement.rs` (`:504`, `:725`, `:832`, `:906`, `:1002`), `rolling/retention.rs:76`, `response.rs:325`, `manager/maintenance.rs:569`, `manager/control.rs:550` and `manager/publication.rs:834`; built by `manager/start.rs` ×2, `manager/cache.rs`, `test_support.rs` and `transcode/tests/chunk_06.rs:398`; about 70 test sites arm it through the manager after it starts. Next, through a `HookSlot` (D-M8-I). **`TranscodeManager`** (`transcode.rs:1332-1354`, `:1525-1530`): unchanged from the #573 row (two pauses, one delay handed to `DecodeFactSource`, two overrides, two records, one fault); after `Session`. **The other `http/hls/` sites:** unchanged from the #573 row; D-M8-J. |
| 2026-09-27 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | Gates | [#579](http://192.168.4.7:3000/noirr/plurx/pulls/579) | On nuc3 in `~/work/hc11`, every `*.rs` under `crates/` and `vendor/` touched first and `Compiling plurxd (/home/pjunod/work/hc11/…)` confirmed in each log. At `9f438b7cb` (base `319569b6a`): `cargo fmt --all -- --check` exit 0; `cargo clippy --workspace --all-targets --locked -- -D warnings` exit 0; `cargo test --locked --no-fail-fast -p plurxd` exit 0 (3,047 passed, 15 ignored; the two new shipped-shape tests, the three new position tests and `seam_census_of_the_workspace` are in its output); `cargo test --release --locked -p plurxd --bin plurxd --` the thirteen owners' shipped-shape tests (these two and the eleven earlier) exit 0 (13 passed; release build 8 m 46 s under a watchdog that would stop it below 1.5 GB free, since nuc3 had 7.4 GB free; the 1.4 GB of release artefacts deleted afterwards). With the ledger and this log: `make history-check` 0, `make validation-lint` 0, validation unittests 0 (248 tests), `make operations-check` 0, `make spike-lock-check` 0. `tests/playback/rolling-producer-owners.toml`, measured from the unittests' actual counts (reviews in the file): `m4-vod-dormant-purge-settlement` 9 → 10, `namespaced-task-spawn` 663 → 668, `method-spawn` 48 → 49, `namespaced-time-constructor` 1070 → 1075, `process-command-construction` 194 → 195, `process-capable-launch-method` 402 → 403, `kill-on-drop-construction` 90 → 91, `process-lifecycle-method` 352 → 325. After merging `main` @ `7a4133860` (#577; conflicts only in the owner ledger, both reviews kept, each count measured by zeroing its row: `namespaced-task-spawn` 670, `method-spawn` 50, `namespaced-time-constructor` 1082, `process-command-construction` 198, `process-capable-launch-method` 406), at `76041d32d`: fmt 0, clippy 0, `cargo test --locked --no-fail-fast -p plurxd` 0 (3,051 passed, 15 ignored), `seam_census_of_the_workspace` 0, history-check 0, validation-lint 0, validation unittests 0 (248), operations-check 0, spike-lock-check 0. The release shipped-shape run above was at `9f438b7cb` and was not repeated after the merge, which touches none of these owners. The whole-tree census figures at the merge include #577's own sites (`cfg_test_census.py` `field` 144, `statement` 130; `syn` `override` 18, `record` 31, `field` 68, `initialiser` 74); the deltas above are this branch's, against `319569b6a`. No web, Android or Apple file changed, so those lanes were not run. No fleet or device step (§6). |
| 2026-09-27 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 / `Session` | [#581](http://192.168.4.7:3000/noirr/plurx/pulls/581) | `a23df92ea`, position tests `da5370963`. **Premise re-verified at `main` `ffe965764`:** `transcode/rolling/session.rs:78-139` held fourteen gated fields: twelve `Barrier`/`LifecycleTestPause` pauses (`activity_detail_pause`, `control_applied_pause`, `flow_completion_pause`, `playlist_publication_pause`, `producer_install_pause`, `refresh_after_read_pause`, `path_owner_sample_pause`, `retention_delete_pause`, `response_projection_pause`, `first_media_owner_claim_pause`, `retirement_cleanup_handoff_pause`, `scratch_cleanup_pause`), the `retirement_started` record, and `replacement_pause`; initialised in the three production constructors (`manager/start.rs:797-825` and `:1402-1430`, `manager/cache.rs:892-920`), `test_support.rs` and `tests/chunk_06.rs`. Sites: `session.rs:1232`, `:1256`/`:1277` (`pause_playlist_publication_for_test`), `:1665`, `:1731`, `:1816`, `:1924`, `:2083`, `:2316`; `rolling/retention.rs:76`; `rolling/retirement.rs:504`, `:725`, `:832`, `:906`, `:1002`; `response.rs:325`; `manager/control.rs:549-671` with its `control_pause` plumbing through `transcode.rs:1717-1783`; `manager/maintenance.rs:571`; `manager/publication.rs:834` and `:1366`. **Premises corrected:** `playlist_publication_pause` was one pause at two sites (compatibility publication and the playlist response's actor observation), so it stays one point at two sites; `retention_delete_pause` was one barrier waited three times (twice before the unlinks, once after the batch), so it becomes two points; `replacement_pause` lived only in the test-only replacement driver (`kill_child_for_replacement`), so it is a `PauseSlot` on `SessionTestHooks`, not a point of the shipped session. **Built:** `seam_hooks::HookSlot<dyn SessionHooks>` in every build: thirteen awaited points and the `retirement_started` record; every point returns a `'static` future, so the detached retention and scratch owners take their point where they are spawned, as the fields were taken. The `control_pause` parameter and field are gone: the point is awaited inside `finish_hls_session_control` on both the direct and the accepted-End path. Race tests kept, moved from `Barrier` pairs and `LifecycleTestPause` to `seam_hooks::AsyncPause` (in `transcode/tests` and `http/hls/tests`). **Shipped shape:** `session_shipped_shape` starts a real copy through `TranscodeManager::start_copy`, asserts the slot reads `NoopSessionHooks`, polls every no-op point ready at its first poll, and drives the session through a published playlist, a served segment, an activity read and a stop whose retirement finishes; the slot is still the no-op at the end. **Census (`validation/cfg_test_census.py`):** `field` 144 → 80, `statement` 130 → 104, `session.rs`, `start.rs` and `cache.rs` 0/0 (`main` measured at `ffe965764`); `seam_census_of_the_workspace` now also asserts `Session` and the functions that held its sites keep no gated site. **Mutations (one scratch build, never committed, in which every move below is compiled in and selected at run time by `PLURX_MUT`; each variant removes the hook from its position and awaits it on the other side of the named step; exit 101 = the test failed, `timeout 170`; the unmutated run of the same binary passed all 35 tests):** path-owner point moved above the replacement-marker check: `path_owner_sample_point_follows_the_marker_check_and_precedes_the_sample` fails (101); moved below the attempt sample: the same test fails (101); *both survived the existing ABA tests*. Compatibility publication point above the first deadline check / below the store: `playlist_publication_point_follows_the_deadline_check_and_precedes_the_store` fails (101 each; both survived before). Playlist-response point above the first-playlist gate: `playlist_response_point_follows_the_first_playlist_gate_and_precedes_the_observation` fails (101; survived before); below the actor observation: that test and `predecessor_playlist_cannot_open_the_successor_startup_gate` fail (101). Producer-install point above the actor authorization, in each of the three install paths: `producer_install_points_follow_authorization_and_precede_the_final_fence` fails (101 each; all three survived before); below the final install fence: the same test fails for the prepublication and pipe paths (101 each; both survived before) and `retirement_after_authorization_cannot_publish_a_replacement` for the replacement path (101). Refresh point above the playlist read: `refresh_point_follows_the_playlist_read` fails (101; survived before); below the projection: `an_older_playlist_read_cannot_land_with_a_newer_catalog_revision` fails (101). Activity point above the actor snapshot: `status_snapshot_never_combines_successor_process_facts_with_predecessor_actor_state` fails (101); below the segment-index read: `activity_point_precedes_the_index_read` fails (101). The step directly after the activity point, `delivery_frontier`, reads only the lease taken before the point when a lease exists, so a move across it alone is equivalent; the `+` side is measured across the next read of mutable state. Control point above the actor acceptance: `cancelled_control_response_cannot_cancel_an_accepted_producer_transition` and `control_and_flow_points_bracket_producer_policy` fail (101); below the flow-ticket wait: `control_and_flow_points_bracket_producer_policy` fails, naming the point after its 10 s bound (101; survived before). Flow point above producer policy: `control_and_flow_points_bracket_producer_policy` fails (101; survived before); below the ticket completion: `retirement_before_flow_completion_cannot_escape_as_active_control` fails (101). Media-committed point above the actor commit: `accepted_predecessor_eof_cannot_project_after_successor_reset`, which now asserts the fetch is committed at the point, fails (101; survived before); below: none, the point is the last statement before the response reports the commit. First-media point above the settlement wait: `first_media_owner_point_follows_an_accepted_settlement` fails (101; survived before); below the latch store: `first_media_settlement_gap_keeps_confirmed_reap_ownership` fails (101). Retirement handoff point above the handoff: `post_media_retirement_keeps_exact_resources_after_caller_cancellation` fails (101); below the transition release: the same test, which now asserts the transition is still held at the point, fails (101; survived before). Retirement record below the transition lock: `stop_waits_for_replacement_and_kills_its_successor` fails (101). Scratch point of the rolling owner below the directory removal: `scratch_cleanup_points_follow_the_release_and_precede_the_removal` fails (101; survived before); the retired-presentation owner's above the release: the same test fails (101; survived before); below the removal: that test and the post-media test fail (101). Retention unlink point below the unlinks: both retention tests fail (101); batch point above the active flag's clear: both retention tests, which now assert the flag is clear at the point, fail (101; survived before). No-op points pending: `session_shipped_shape` fails (101). **Not expressible:** the scratch owner's and the retention worker's `-` side (each point is the first statement of a task spawned from a synchronous function), and the `+` side of the media-committed and retention-batch points (each is the last statement before return). |
| 2026-09-27 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 / `TranscodeManager` | [#581](http://192.168.4.7:3000/noirr/plurx/pulls/581) | `d9c2e315c`. **Premise re-verified at `main` `ffe965764`:** `transcode.rs:1332-1354` and `:1525-1530` held eight gated fields, initialised in `manager/construct.rs:65-77` and `:120-122`. Classified: **pauses** `subtitle_playlist_commit_pause` (awaited in `http/hls/subtitle_playlist.rs` through `manager/maintenance.rs:454-462`) and `vod_publication_admission_pause` (`manager/publication.rs:390`); **overrides** `force_artifact_qualification` (`manager/plan.rs:161`; cleared by the real publisher, `describe.rs:260`) and `offline_produce_script` (`manager/produce.rs:702`); **record** `offline_produced_recipes` (the recipe a scripted attempt received) and `manifests_published` (`produce.rs:1318`); **fault** `fail_next_offline_recovery_begin` (`produce.rs:507`, with a `cfg(not(test))` twin of the Store write); **plumbing** `decode_source_final_identity_delay` (`plan.rs:377`, handed to `DecodeFactSource`). **Built:** `seam_hooks::HookSlot<dyn TranscodeManagerHooks>`: the two pauses awaited where the barriers were (the serving authority is still checked on both sides of the VOD point); `forces_artifact_qualification` and `scripted_offline_outcome` answer in place of production work (`false` and `None` in production; the scripted outcome records its recipe); `offline_recovery_begin_fault` returns the injected error or `None`, and the twin is one `match`; `artifact_qualification_published` and `manifest_published` are records; `decode_fact_source` returns the source unchanged in production. The test accessors and the `with_decode_source_final_identity_delay` builder keep their signatures over `TranscodeManagerTestHooks`, so their callers are unchanged; the two pause tests move to `AsyncPause`. **Claimed not migrated, withdrawn in the review round (row below):** `resolve_movie_plan_with_qualification`'s test-build decoder fallback (`plan.rs:249`) was said to be relied on by every test-built manager, rather than armed by one test. That was false: the review measured one test depending on it, and the fallback is deleted. **Shipped shape:** `transcode_manager_shipped_shape` builds the manager through the production constructor, asserts the slot reads `NoopTranscodeManagerHooks`, polls both pause points ready at their first poll, checks the overrides and the fault answer as production does, and runs the real qualification publisher through its record point (it drove no migrated call site; extended in the review round below). **Census:** `field` 80 → 64, `statement` 104 → 96 (`transcode.rs` 9/1 → 0/0, `manager/construct.rs` 8/0 → 0/0). The `syn` census (`seam_census_of_the_workspace`) from `main` `ffe965764` to the branch, both owners: `pause` 32 → 17, `fault` 17 → 16, `override` 18 → 16, `record` 31 → 25, `plumbing` 30 → 21, `field` 68 → 45, `initialiser` 74 → 23, `arm` 19 → 19. **Mutations (same method):** subtitle point above the track resolution: `subtitle_playlist_point_follows_the_track_resolution` fails (101); below the commit: `published_subtitle_playlist_refuses_in_place_video_attempt_handoff`, which now asserts the response has not committed at the point, and `real_subtitle_playlist_cannot_commit_after_vod_same_id_reattachment` fail (101). VOD point above the serving admission: `vod_publication_point_follows_the_serving_admission` fails (101); below the owner check: `in_flight_vod_publication_rechecks_direct_serving_authority` fails (101). No-op points pending: `transcode_manager_shipped_shape` fails (101). The unmutated run passed all 7 tests. |
| 2026-09-27 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 decisions | [#581](http://192.168.4.7:3000/noirr/plurx/pulls/581) | **Scope (a deviation, recorded):** `Session` and `TranscodeManager` in one PR, each its own commit and each proved separately, with the `Session` position tests in their own commit; #579 had planned them as separate PRs. **Method:** the position mutations were run from one scratch build that compiles every move in and selects one at run time (`PLURX_MUT`), rather than one build per move: each variant still removes the hook from its position and awaits it on the other side of its step, and the same binary with no selection passed every test it was run with. **Recorded, not a new decision:** the test-only replacement driver's pause is test code, so it is not a `SessionHooks` point; `TranscodeManager`'s test-build decoder fallback was recorded as not a hook; that reason was false, and the review round below deletes the fallback. No fleet or device step. |
| 2026-09-27 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | Gates | [#581](http://192.168.4.7:3000/noirr/plurx/pulls/581) | On nuc3 in `~/work/hc6` at `8910a4a10` (base `main` `ffe965764`), every `*.rs` under `crates/` and `vendor/` touched first and `Compiling plurxd (/home/pjunod/work/hc6/…)` confirmed: `make history-check` 0, `make validation-lint` 0, validation unittests 0, `make operations-check` 0, `make spike-lock-check` 0, `cargo fmt --all -- --check` 0, `cargo clippy --workspace --all-targets --locked -- -D warnings` 0, `cargo test --locked --no-fail-fast -p plurxd` 0 (3,064 passed, 15 ignored). Release profile: `cargo test --release --locked -p plurxd -- session_shipped_shape transcode_manager_shipped_shape` 0 (2 passed). No client or web code changed. |
| 2026-09-27 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 review round | [#581](http://192.168.4.7:3000/noirr/plurx/pulls/581) | The one adversarial review (comment 5815) at `6361e4f14`; `main` had not moved (`ffe965764`), so no merge. **Finding 1 (P1), fixed in `88b48e1c5`:** the claim that every test-built manager relies on `resolve_movie_plan_with_qualification`'s test-build decoder fallback was false. One test depended on it (`a_measured_decoder_names_the_plan_only_where_it_is_enforced`), which now names its decoder through `with_decoders`. The fallback is deleted, so test and release managers resolve decode capabilities from the same inventory. `TranscodeManager` now meets §5.9's zero-count acceptance (`manager/plan.rs` 0/1 → 0/0). `seam_census_of_the_workspace` asserts `TranscodeManager` keeps no test-only site with `is_empty()`; the old `.all(..)` over a filtered list would have passed on an empty list. `transcode_manager_sites_are_recognised_wherever_the_manager_gates` pins the owner predicate against a gated field, a gated method statement and a gated hooks-trait default. *Proved:* with the fallback restored in the source, `seam_census_of_the_workspace` fails naming `TranscodeManager::resolve_movie_plan_with_qualification`, and passes with it removed. With the fallback gone and the test's `with_decoders` removed, `a_measured_decoder_names_the_plan_only_where_it_is_enforced` fails (`left: None`). **Finding 2 (P2), fixed in `b510c2f06`:** `transcode_manager_shipped_shape` now drives the migrated call sites through the production no-op hooks on a production-built manager. (a) The real qualification publisher. (b) Plan resolution under an enabled, partially covered policy: the covered hardware path qualifies and the uncovered software path does not, which a plan forced onto the published identity would not produce. (c) An unscripted offline production through `ensure_offline`, which reaches the real producer, yields at its expired deadline and pins its recipe. (d) A VOD publication through the serving admission and its point. (e) A held-source plan through `decode_fact_source`, bound to the held descriptor. Its HTTP half, `transcode_manager_shipped_shape_subtitle_playlist` (`http/hls/tests/chunk_02.rs`), commits a real VOD subtitle playlist through `before_subtitle_playlist_commit`. The `transcode_manager_shipped_shape` filter selects both tests. *Proved* with one scratch build, never committed, with each miswiring selected at run time by `PLURX_MUT` (the unmutated binary passed all three tests it was run with). Each miswiring fails the shipped-shape test: the inverted `forces_artifact_qualification` check at `plan.rs:159` (the hardware plan does not enforce its receipt); an unscripted offline outcome answered in place of production at `produce.rs:684`; the held plan resolved from the catalog row instead of through `decode_fact_source` (`left: CatalogRow`); a VOD publication point that never resolves (both halves time out); a subtitle commit point that never resolves (the HTTP half times out). **Finding 3 (P2), fixed in `7a76a9cb9`:** `SessionHooks::before_retention_unlink`'s doc now says the worker started, before it reads its pass and unlinks anything, so an entry queued while the point is held is part of that pass. This is a doc change; behaviour is unchanged. **Census:** `validation/cfg_test_census.py` `field` 64, `statement` 96 → 95. The `syn` census `record` 25 → 24; the other classes are unchanged. **Owner ledger:** `namespaced-time-constructor` 1101 → 1104, measured, with the review in the row (three test-only bounds). **Gates** on nuc3 in `~/work/hc6` at `7a76a9cb9`. Every `*.rs` under `crates/` and `vendor/` was touched first, and the log shows `Compiling plurxd v0.3.0 (/home/pjunod/work/hc6/crates/plurxd)`. Results: `make history-check` 0, `make validation-lint` 0, validation unittests 0, `make operations-check` 0, `make spike-lock-check` 0, `cargo fmt --all -- --check` 0, `cargo clippy --workspace --all-targets --locked -- -D warnings` 0. `cargo test --locked --no-fail-fast -p plurxd` 0 (3,066 passed, 15 ignored). Release `cargo test --release --locked -p plurxd -- session_shipped_shape transcode_manager_shipped_shape` 0 (3 passed). **Sibling:** #583 (`plan/S-14-9`) also touches the census test and the owner ledger, so whichever PR lands second re-measures. |
| 2026-09-27 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 / `http/hls/` route group | [#583](http://192.168.4.7:3000/noirr/plurx/pulls/583) | `ae7955416` and the position tests `aff55e0d1`, branched from `main` @ `ffe965764` (#579 and #580 merged; #581, `Session` and `TranscodeManager`, in review in parallel). **Premises re-verified at `ffe965764`:** every function that holds a site takes `AppState`: `control.rs:523` `settle_preparation_control` (settlement delay `:544-554`, settlement fault `:699-706`), `:2261` `control_local_with_settlement_capacity` (staged-read fault `:2367-2378`, dispatch wait `:2971-2976`), `create.rs:1239` `create_with_purpose` (ask-recorded pause `:1318-1330`), `preparation.rs:1117` `process_preparation_candidate` (completion record `:1137-1149`, planning fault `:1199-1206`, the `#[cfg(not(test))]` twin `:1292-1327` with `stage_and_prime_prepared_successor` `:1422`), `:1451` `stage_prepared_successor_with_prime` (registration delay `:1718-1721`), `release.rs:161` `release_session` (pauses `:175-179`, `:253-257`), `:347` `end_media_session_for_release` (fault `:352-357`), and `status.rs:11` `status_local_before_with_relay` (record `:38-39`), which #579's row did not list. Their keyed statics were `control.rs:310-339` and `:1837-1966`, `create.rs:158-167`, `preparation.rs:520-534`, `release.rs:364-421` and `status.rs:155-175`. **Premise corrected:** the #573 row counted `subtitle_playlist.rs` as one record; it is `TranscodeManager::pause_subtitle_playlist_commit_for_test()`.await`, an awaited pause the `syn` census files as a record, owned by `TranscodeManager`, which #581 migrates; it is not moved here. **Built:** `http/hls/hooks.rs`: `HlsRouteHooks` (`Any` supertrait), `NoopHlsRouteHooks`, and `HlsRouteTestHooks` under `#[cfg(test)]`, held as `AppState::hls_route_hooks: Arc<HookSlot<dyn HlsRouteHooks>>`. Seven awaited points (`after_create_ask_recorded`, `before_dispatch_answer`, `before_preparation_settlement`, `before_preparation_planning`, `before_preparation_registered`, `after_release_fence_closed`, `after_release_tombstoned`), four faults as return values, two records (`preparation_candidate_finished`, from a drop guard on every exit, and `status_local_lookup`) and `primes_prepared_successor`, which replaces the twin. The arming helpers keep their names and take the state they arm. The create pause and both release pauses are `AsyncPause`s (the release pauses were `Barrier` pairs); the dispatch wait gives up after `ASYNC_PAUSE_BOUND`; the three delays stay delays. **Shipped shape:** `hls_route_shipped_shape` builds the state through `AppState::new_unhooked` (the production constructor, no test hooks), asserts the slot reads `NoopHlsRouteHooks`, polls all seven points ready at their first poll, reads each fault (`false`) and the prime choice (`true`), and runs a release through its three points to a durable End. The status observer test now also counts one lookup of an owned route, so its zero is an absence rather than a point that never records. **Census:** `validation/cfg_test_census.py` `statement` 130 → 115 (the five route files 15 → 0: `control.rs` 5, `create.rs` 1, `preparation.rs` 5, `release.rs` 3, `status.rs` 1), `field` 144 unchanged; the `syn` census (353 production files, `hooks.rs` the new one) `pause` 32 → 24, `fault` 17 → 15, `record` 31 → 29, `plumbing` 30 → 27 (`override` 18, `field` 68, `initialiser` 74 and `arm` 19 unchanged: the twin was `#[cfg(not(test))]`, which the census does not count). `seam_census_of_the_workspace` now asserts `http/hls/` keeps no gated site except `subtitle_playlist.rs`'s `TranscodeManager` pause; `hooks.rs` joins `HLS_PRODUCT_SOURCES`. **Mutations** (one scratch build, never committed, every move compiled in and `PLURX_MUT` selecting one at run time; each run is the `http::hls` tests; exit 101 = a test failed): every no-op point `pending()`: `hls_route_shipped_shape` fails (101). `after_create_ask_recorded` above the ask's recording and below the activation that carries the recorded revision: `a_create_is_refused_when_the_ask_moves_before_it_activates` fails under both (101 each). `before_dispatch_answer` above the dispatch: `delivery_preparation_says_staging_on_the_dispatch_after_its_candidate_is_gone` fails (101); below the answer: `dispatch_answer_point_precedes_the_pending_map_read`, written with the migration for this side (an exchange that dispatches nothing, held at the point while a candidate for its own ask appears, must answer `staging`), fails (101). `before_preparation_settlement` below the first commit: `a_timed_out_settlement_finishes_detached_and_replays_durably` and `admitted_preparation_settlement_holds_serving_fence_through_commit_and_abort` fail (101). *Survived as first written:* above the retry deadline passed (0); `a_settlement_delayed_past_its_retry_budget_expires_uncommitted` (a delay past the whole 5 s budget expires the settlement uncommitted) now fails under it (101). `before_preparation_planning` below the post-planning cancellation check: `delivery_preparation_stays_staging_while_the_candidate_is_planning` fails (101). Above the source read passes (0), recorded and not pinned: the delay's one property is that the candidate is parked inside the pending window before it can reach the ledger, and every earlier position is inside the same window. `before_preparation_registered` *survived as first written:* below `register_active_preparation` passed (0); `a_second_ask_supersedes_a_preparation_in_the_registration_window` now asserts nothing is registered while the task is parked, and fails under it (101). Above the priming block passes (0), recorded and not pinned: for a selection change the planned-outage re-check it moves above does not run, and any earlier position is still before registration, where the claim the post-registration re-read checks persists. `after_release_fence_closed` *survived as first written, both ways* (above the publication fence, below the durable End: 0 each); `release_fence_point_sits_between_the_fence_and_the_durable_end` (while held, the fence is closed, read through the new test-only `TranscodeManager::session_publication_fenced_for_test`, and the durable route is still `active`) fails under both (101 each); its first version read the fence through a playlist request, which refused under the above-the-fence move too, so it was replaced. `after_release_tombstoned` *survived as first written:* below the remote owner's terminal projection passed (0); `release_tombstone_point_sits_between_the_durable_proof_and_the_remote_owner` (route owned by an unreachable node; while held the ended row's projection is armed, and the release is deferred only after the point) fails under it (101). Above the durable proof (`complete_release_with_route`): that test and `durable_delete_tombstone_blocks_media_before_local_stop` fail (101). Above the local gate projection (`complete_session_release_durable`, synchronous, no await between it and the point) passed as first written (0); since the #583 review round the same test asserts the local gate no longer reads as an active release while held (`session_publication_fenced_for_test`), and fails under it (101). |
| 2026-09-27 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 decisions | [#583](http://192.168.4.7:3000/noirr/plurx/pulls/583) | **Decision D-M8-J executed as recorded.** **Decision D-M8-K (Paul can overturn it): the test-only `AppState::new` fills the route group's slot with `HlsRouteTestHooks`.** D-M8-I fills a slot the first time a test arms a point; here one of the route group's answers applies without arming: every test build staged an admitted candidate without priming (the `#[cfg(not(test))]` twin), and the preparation decision tests rely on it. `AppState::new` turns the test hooks' priming off (`prime_prepared_successors(state, false)`), so a test-built state behaves as before; `AppState::new_unhooked` is the same constructor without them, for the shipped-shape test. Since the #583 review round, installing the test hooks on a state (arming any point) starts them answering priming as the hooks they replace did, so arming a point on a production-built state keeps it priming. States built through `new_configured` directly in tests (`main.rs`'s `build_state` test) read the no-op and would prime; none of them reaches a preparation candidate. **To overturn:** tests default to priming and each preparation test that must not launch ffmpeg arms stage-only first. **Delays kept as delays:** the settlement, planning and registration points hold for a duration the test arms, as before: those tests race real exchanges against a window of a fixed length, and an `AsyncPause` would rewrite them; a delay is bounded by its duration. |
| 2026-09-27 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M3 remainder | [#583](http://192.168.4.7:3000/noirr/plurx/pulls/583) | Not taken. The declarations-only move of `StartInfo`, `probe_media_origin` and the shared helpers out of `transcode.rs` (1,857 lines at `ffe965764`, nearly all of it outside its split blocks) is a move PR under §3.1's rules, with its own identity recipe; it cannot share a PR with this redesign, and #581, in review, edits `transcode.rs` (+183/−40), so a move now would conflict with it. It stays a follow-up move PR, best taken after #581 merges. |
| 2026-09-27 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | Gates | [#583](http://192.168.4.7:3000/noirr/plurx/pulls/583) | On nuc3 in `~/work/hc11` at `1ca0921f2` (base `ffe965764`; commits `ae7955416`, `aff55e0d1`, `1ca0921f2`), every `*.rs` touched first and `Compiling plurxd (/home/pjunod/work/hc11/…)` confirmed: `make history-check` 0, `make validation-lint` 0, validation unittests 0 (248), `make operations-check` 0, `make spike-lock-check` 0, `cargo fmt --all -- --check` 0, `cargo clippy --workspace --all-targets --locked -- -D warnings` 0, `cargo test --locked --no-fail-fast -p plurxd` 0 (3,056 passed, 15 ignored; the five new tests are in its output); release `cargo test --release --locked -p plurxd --bin plurxd hls_route_shipped_shape` 0 (release build 7 m 24 s, 1 passed). The `http::hls` tests (226) also passed in each mutation build with no selection. No web, Android or Apple file changed, so those lanes were not run. No fleet or device step (§6). #581 (`Session`, `TranscodeManager`) edits the same census test, the owner ledger and `http/hls/tests/chunk_01.rs`; whichever merges second merges `main`, re-measures the ledger rows and drops `subtitle_playlist.rs`'s exception from `seam_census_of_the_workspace` once #581's migration of that pause is in. |
| 2026-09-28 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 / `http/hls/` review round | [#583](http://192.168.4.7:3000/noirr/plurx/pulls/583) | The adversarial review (comment 5870) found four P2 gaps in evidence, none in the production path; each is fixed. **1, no test ran the production priming arm** (`7c464f5f3`): installing the route test hooks on a state, which arming any point does, installed `HlsRouteTestHooks::default()` and silently turned priming off, and the comment said a test could ask for priming when nothing could. The test hooks now start out answering `primes_prepared_successor` as the hooks they replace did; `AppState::new` turns priming off through the new `prime_prepared_successors(state, bool)`, which the corrected `preparation.rs` comment names. A pause before a priming successor registers (`pause_before_preparation_registered`), which only the priming path reaches, lets `hls_route_shipped_shape` drive an admitted candidate through `process_preparation_candidate` on an `AppState::new_unhooked` state (`HlsDeliveryFixture::publish_unhooked`, `staging_route_on`): it asserts arming kept priming, holds the successor at the point, supersedes it there so it tears itself down without launching an encoder, and checks that `AppState::new`'s state declines until the helper turns priming on. Proved: production never priming (`&& false` on the prime choice) fails it at `reached` (the owner never reaches the pause, 101); test hooks installed with priming off fail it at the arming assertion (101). **2, tombstone move recorded as not pinned** (`dc5f6e888`): the reviewer's assertion (`!session_publication_fenced_for_test` while held) is in `release_tombstone_point_sits_between_the_durable_proof_and_the_remote_owner`; the point above `complete_session_release_durable` fails it (101). The #583 mutation row above now says so. **3, the settlement fault was pinned by no test** (`16db01aba`): `a_transient_store_failure_does_not_strand_a_rolling_commit` asserts the armed failure was taken on the fixture's state (`untaken_preparation_settlement_faults` reads 0); `preparation_settlement_fault` always `false` fails it (101). **4, census exception wider than stated** (`6a83c69e9`): #581 had not merged after 90 minutes of polling (its promotion gate failed at `0e076e696`), so `main` was merged without it and the exception stays, narrowed. The route group has its own assertion over `http/hls.rs` and everything under `http/hls/`, exempting only `complete_subtitle_playlist_response`'s awaited `TranscodeManager` pause by file, owner and seam together, at most one site. `http_hls_route_sites_are_recognised_in_the_parent_and_every_child` shows a new gated site in `http/hls.rs`, in a new file under `http/hls/` or elsewhere in `subtitle_playlist.rs` is counted; with the old file-wide filter it fails (101). Whichever of #581 and #583 merges second deletes `awaiting_transcode_manager_migration` and asserts zero. Mutations 1–3 ran in one scratch build at `16db01aba` (`PLURX_MUT` selecting each; unselected, all three tests pass), 4 in one at `6a83c69e9`; none committed. **Merge:** `main` @ `dfef993b9` in `51dff93bb`; one conflict, the owner ledger's `namespaced-task-spawn` (673: main 669, this branch +3, this round's joined candidate task +1) and `namespaced-time-constructor` (1090: main 1088, this branch +1, this round's ten-second join bound +1), each measured by zeroing its row, reviews in the file. The merge was built and tested before the census commit: `cargo test -p plurxd` 0 (3,050 passed, 15 ignored). **Census at `6a83c69e9`:** `cfg_test_census.py` `statement` 115, `field` 144; `syn` (355 production files) `pause` 24, `fault` 15, `override` 18, `record` 29, `plumbing` 27, `field` 68, `initialiser` 74, `arm` 19; this round adds only `#[cfg(test)]` items. **Gates** on nuc3 `~/work/hc11` at `6a83c69e9`, every `*.rs` touched first and `Compiling plurxd (/home/pjunod/work/hc11/…)` confirmed: `cargo fmt --all -- --check` 0, `cargo clippy --workspace --all-targets --locked -- -D warnings` 0, `cargo test --locked --no-fail-fast -p plurxd` 0 (3,051 passed, 15 ignored); release `cargo test --release --locked -p plurxd --bin plurxd -- shipped_shape` 0 (14 passed, the fourteen owners' shipped-shape tests; release build 7 m 20 s). With this row: `make history-check`, `make validation-lint`, validation unittests, `make operations-check`, `make spike-lock-check` (results in the PR disposition). No web, Android or Apple file changed. No fleet or device step (§6). |
