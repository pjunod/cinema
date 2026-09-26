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
`std::sync::Barrier` and neither build has an await point. The hook trait
has `Any` as a supertrait so a test can reach the pauses installed on a
child it did not construct (the test-only `AttemptChild::new` installs
them; production `new_with_job` installs the no-op). `DecodeFactSource`'s
`#[cfg(not(test))]` getter twins are gone: both builds read the delay from
the hook. The census script above now accepts any `pub(...)` restriction;
it matched only `pub(crate)`, so `pub(super) fn` lines were counted as
statements. Where a seam is not a pause
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
of `Encoding`, whose design question is in the execution log). One owner
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
| 2026-09-26 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 / `AttemptChild` | [#556](http://192.168.4.7:3000/noirr/plurx/pulls/556) | `f83df478d`. **Premise re-verified:** `transcode/producer/attempt_child.rs` held the three `#[cfg(test)]` pause fields (`:178-185` at base `91f363154`), six `#[cfg(test)]` clones of them (`:261-275`), three gated pause blocks in the supervisor (authorization `:314-322` and flow reservation `:335-345`, both inside the producer transition fence and blocking on a `std::sync::Barrier`; before-reap `:387-395`, an awaited `Notify` pair) and the gated initialisers (`:451-456`); every test construction went through the test-only `AttemptChild::new`, and the pauses were installed after construction (`pause_signal_after_*`, and the `terminate_before_reap_pause` field in three `chunk_06.rs` tests). All three seams are ordering pauses. **Built:** one `Arc<dyn AttemptChildHooks>` held by the child in every build, the supervisor running through a clone taken from it; production `new_with_job` installs `NoopAttemptChildHooks`, the test-only `new` installs `AttemptChildPauses` (reached by `Any` upcasting). The two fence points are synchronous hook methods (no await point there in either build, as before); the before-reap point is an awaited `HookFuture` whose no-op is the zero-sized `HookReady` #543 introduced, now shared. **Deviations, recorded:** `Arc` instead of #543's `Box` (two holders), and two owners in one PR (§5.9 says one per PR; each owner is its own commit). Census (`validation/cfg_test_census.py`, pattern corrected in `f8337377b` to accept `pub(super)`; base re-measured with the corrected script): `attempt_child.rs` `field` 3 → 0, `statement` 10 → 0 (under the old pattern: 2 → 0 and 16 → 7, the 7 being `pub(super) fn`/`struct` lines it mis-filed). Race tests kept through the hooks and green: `producer_signal_and_retirement_share_one_authorization_linearization`, `actor_task_exit_fences_a_reserved_signal_and_cleanup_still_progresses`, `published_failure_cleanup_survives_waiter_cancellation_and_retains_media`, `prepublication_retirement_holds_admissions_until_confirmed_reap`, `first_media_settlement_gap_keeps_confirmed_reap_ownership`. Shipped shape: `attempt_child_shipped_shape` runs `new_with_job` through all three points (suspend and resume publish the held and running flow for the attempt; a termination request reaches the published reap within 5 s). Mutations, each run: the production before-reap hook never ready fails the shipped-shape test (exit 101); the authorization hook moved outside the transition fence makes the linearization race test never complete (it reported running over 60 s and was killed, exit 101) — a hang, not a clean assertion, because the test's own `std::sync::Barrier` is what the moved hook no longer meets. `tests/playback/rolling-producer-owners.toml`, measured by zeroing each row: `supervisor-registration` and `rolling-supervisor-construction` 22 → 23, `namespaced-task-spawn` 639 → 640, `namespaced-time-constructor` 1009 → 1010, all the shipped-shape test's, reviewed in the file. No fleet or device step (§6). |
| 2026-09-26 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | §7 Q3 (`syn` pass) | [#556](http://192.168.4.7:3000/noirr/plurx/pulls/556) | `f8337377b`: `crates/plurxd/src/transcode/tests/seam_census.rs`, run with `cargo test -p plurxd --bin plurxd seam_census -- --nocapture`. Answer recorded in §7 Q3: after this PR, 158 gated statements in shipping functions — 48 pause, 17 fault (7 of them `dv_disk.rs`), 15 override, 34 record, 44 plumbing — plus 97 gated fields and variants, 104 initialisers and 19 arms, over 310 production files (33 test-only files skipped). Two fixture tests pin the classifier (every class, `#[cfg(all(test, ..))]`, and test items ignored) and the test-file resolution (`#[cfg(test)] mod`, `#[path]`, `include!` from a test file); `seam_census_of_the_workspace` asserts the migrated owners (`AttemptChild`, `RollingRetirementSettlement`, `DecodeFactSource`) keep no gated sites. The fixture spells `cfg(TEST)` and swaps it before parsing so the line census does not count fixture text. `m4-exact-rolling-retirement-settlement` 8 → 9 (the owner name as a string in that assertion), measured and reviewed. |
| 2026-09-26 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M8 / `DecodeFactSource` | [#556](http://192.168.4.7:3000/noirr/plurx/pulls/556) | `bc1c632ee`. **Premise re-verified:** `decode_facts.rs` held `#[cfg(test)] initial_identity_delay` / `final_identity_delay` fields (`:640-643` at base), their gated initialisers (`:656-659`) and `#[cfg(test)]`/`#[cfg(not(test))]` getter twins (`:684-702`) read at the two identity observations (`:3400`, `:3496`), which hand the delay to the blocking syscall owner that sleeps only when it is non-zero. Delays installed by consuming test builders on a source the test holds, so the hook is chosen before use. **Built:** `Arc<dyn DecodeFactSourceHooks>` (the source is `Clone`), production `NoopDecodeFactSourceHooks` (both zero), test `IdentityDelays` installed by `with_identity_delay` / `with_final_identity_delay`, each keeping the delay it does not set; the twins are gone. Census: `decode_facts.rs` `field` 8 → 4 (the owner's two fields and two initialisers; the four left belong to other owners in the file), `statement` 13 unchanged (other owners). Race test `blocked_source_identity_returns_deadline_without_releasing_probe_ownership` kept, and the manager tests that use `with_final_identity_delay` (`source_change_refuses_bound_plan`, `prepared_plan_keeps_producer_execution_out_of_the_probe_lane`) pass in the full suite. Shipped shape: `decode_fact_source_shipped_shape`, the race test's scenario with `DecodeFactSource::new`, returns the fixture probe's `InvalidJson` inside the 100 ms budget with the probe lane free. Mutations, each run (exit 101): a production initial delay of 1 s fails the shipped-shape test; a getter ignoring the hook fails the race test. **Taken ahead of `Encoding.admission_pause`:** that owner's race tests (`vod/tests/chunk_03.rs` ×3, `vodencode_tests.rs`) install the pause after construction on `Encoding`s production code builds (`transcode/manager/create.rs:966`, reached through the VOD recipe fixtures), and on one specific `Encoding` of a predecessor/successor pair. A hook held by an `Encoding` cannot be replaced once it is inside its `Arc`, so the options are a test-build constructor twin (what M8 removes) or a hook factory held by `TranscodeManager` (itself an owner with eight test-only fields) that can target one rendition — a design choice for the `Encoding` PR, not guessed here. |
| 2026-09-26 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | Gates for #556 | [#556](http://192.168.4.7:3000/noirr/plurx/pulls/556) | On nuc3 in `~/work/hc4` at `bc1c632ee` (base `91f363154`; `main` has since gained only #552's one-line K-09 board edit): `cargo fmt --all -- --check` exit 0; `cargo clippy --workspace --all-targets --locked -- -D warnings` exit 0; `cargo test --locked --no-fail-fast -p plurxd` exit 0 (2,957 passed, 14 ignored, compiled from this worktree); `cargo test --release --locked -p plurxd --bin plurxd -- attempt_child_shipped_shape decode_fact_source_shipped_shape rolling_retirement_settlement_shipped_shape` exit 0 (release build 10 m 10 s, 3 passed), release artefacts deleted afterwards (disk under 20 GB); `make history-check` 0, `make validation-lint` 0, validation unittests 0 (528), `make operations-check` 0, `make spike-lock-check` 0. **Remaining M8 owners:** `Encoding.admission_pause` (design question above), the `playback_control.rs` owners, the `http/hls/` sites, the `dv_disk.rs` faults (a `fail`-feature candidate, §7 Q3), and the owners the census lists that §5.9 does not name (`TranscodeManager`, `Session`, `LiveTvManager`, `JobManager`, VOD `Shared`/`Rendition`). |
