# Shared Dolby Vision and HDR — deliver every local shape over sharing

**Status:** in progress (M1 and T2.1 on `claude/shared-dv-hdr-m1`) · **Branch:** `effort/shared-dv-hdr` · **Written:** 2026-11-15 ·
**Verified against:** origin/main @ `29b35e713` (read, not compiled) ·
**For:** the build agent (Opus). Nothing here is for review by another model.

Companion to `docs/streaming/SHARED-LIBRARY-PLAYBACK-RCA.md`
(why shared starts fail) and `docs/features/SHARED-LIBRARIES-IMPLEMENTATION.md`
§"Dolby Vision conversion stays a typed limitation" (`:6304-6309`), which this
plan retires.

Work milestone by milestone; each task is one small PR onto the effort branch.
Line numbers are from `29b35e713` — re-find by symbol at build time. If a task
seems to need a new refusal, a watchdog, or a setting that gates the feature,
stop and flag it instead: the rule for this effort is that the Source plans
the next-best delivery through the shared planner and never refuses a shape
local playback serves.

Paths below are relative to `crates/plurxd/src/` unless they start with
`crates/`, `clients/` or `docs/`.

## 1. Root cause — the pipeline exists; policy and four closed vocabularies block it

The Source already plans with the shared planner
(`http/hls/create.rs:6815-6839`, `review_client_plan` +
`resolve_plan_for_principal`), and `review_client_plan` **derives**
`preserve_dolby_vision` / `convert_dolby_vision` from the caps
(`create.rs:1140-1146`); the client's ask only clamps. So a DV-capable client
on a shared DV title always produces `Copy { preserve_dolby_vision: true }`,
which refusal 1 rejects. The receiver maps it to a terminal error
(`http/shared_receiver_playback.rs:143`, `:1900`). **A DV-capable client gets
no playback of any shared DV title.** A non-DV client gets the HDR10 base only
for a compatible-base title with no native subtitle, and only on the growing
path (§1.2).

### 1.1 The three refusals (`transcode/source_actor.rs:2165-2197`)

| # | Refused shape | Stated reason | What is actually missing |
|---|---|---|---|
| 1 | Copy with `preserve` or `convert` | none in code; doc says conversion's "per-source RPU proof is not Source-owned" (`IMPLEMENTATION.md:6304`) | **Nothing physical.** P7→P8.1 is `dvpipe::Converter`, an in-process fragment rewriter with no child and no proof. The Source growing path already builds it: `growing.rs:419-425` passes both flags into `CopyVideoOptions`, and `copy_normalization.rs:47-58`, `:100` create the converter / record rewrite. The Source master already carries the facts (`growing.rs:236-256`, `source_actor.rs:2231-2241` → `copied_hls_codecs` → `SUPPLEMENTAL-CODECS`, `http/hls/playlist_text.rs:284-305`). Missing are the items in §1.2–§1.4. |
| 2 | Any DV file that is encoded, or has native subtitles | "the RPU renderer's per-source proof is not Source-owned work yet" (`source_actor.rs:2149-2151`) | The proof is `require_dovi_renderer_with_offline` (`transcode/manager/plan.rs:1159-1228`): two ffmpeg probes spawned in a detached `tokio::spawn` (`transcode/dovi_proofs.rs:103`) that open their own fence (`ffmpeg.rs:4697`). That is unowned work for a Source. But it is required **only for Profile 5** (`needs_dovi_reshape`, `plan.rs:1117-1127`): a compatible-base title (P7, P8.1, P8.4) returns `Ok(false)` and encodes through the ordinary HDR10/HLG base route. So the refusal is far wider than its reason. Native subtitles beside DV need nothing DV-specific at all. |
| 3 | Native subtitles over any HDR source | none | **Nothing.** `source_native_master` is the shared master generator (`playlist_text.rs:567-581`) and emits `VIDEO-RANGE`, `CODECS`, `SUPPLEMENTAL-CODECS`. The refusal predates S5 (#1490). |

The same policy is re-checked in the workers (`source_actor.rs:1588-1601`,
`growing.rs:690-700`) and in `prepare_source_start`
(`http/shared_source_direct.rs:324-336`).

### 1.2 Index identity — the Source never matches the local index of a DV title

`have_dovi` (ffmpeg has `dovi_rpu`) is part of the copy argv and therefore of
the fragment-index identity (`crates/plurx-core/src/transcode/mod.rs:340-378`,
`fragindex.rs:1232`). Local uses the real value; the Source hard-codes `false`
in three places:

- `vod/serve/create.rs:2660-2664` — "a generic capability probe here would
  create an unowned child". Stale: the value is measured once at boot
  (`main.rs:3280`) and held as `TranscodeManager::dv_strippable()`
  (`transcode/manager/construct.rs:298`) and `state.system.dovi_rpu`.
- `transcode/source_actor/growing.rs:419-424`
- `http/stream.rs:2566` (`local_derivation && …`), which also makes
  `vod_indexed` false in the shared decision.

Consequences: a non-preserved DV copy has a different identity from the one
local analysis built, so `FullVod` always misses the index; and the copy is
cut with `filter_units` only, leaving a stale `dvcC`
(`mod.rs:410-417`), where local strips with `dovi_rpu=strip=1`. In addition
`prepare_source_cold_index` skips DV (`source_actor.rs:1288-1292`) and
`start_source_index` refuses it (`fragindex.rs:2045-2051`), so a DV title with
no index always ends `vod_index_pending` on `FullVod` (RCA §2.1).

### 1.3 Copy-head validation refuses a DV range

`validate_prepared_copy_head` (`create.rs:6933-6944`) maps the delivered range
to a transfer and returns `Err` for anything but `hdr10` / `hlg` / `sdr`. It is
called on every growing start (`growing.rs:521`). With refusal 1 lifted, every
preserved or converted copy would fail here as `vod_source_rescan_required`.
It also checks `entry.present.contains(transfer)` and the HEVC sample entry
against `hls_hevc_sample_entries`, with no DV arm (`dvh1`, `dv_profiles`).

### 1.4 B's relay — two closed vocabularies and one early refusal

B does not drop DV/HDR facts from playlists: `validate_sharing_playlist`
admits `VIDEO-RANGE` and `SUPPLEMENTAL-CODECS`
(`crates/plurx-core/src/sharing_resources.rs:436-447`) and version 10 masters
(`:630`). The decision wire carries all DV fields
(`http/sharing_decision_decode.rs:183-189`). What blocks DV on B:

- `http/shared_receiver_ingress.rs:143-152` refuses an explicit
  `preserve_dolby_vision: true` before dispatch.
- `validate_response` (`http/sharing_start_decode.rs:208-217`) rejects a Start
  whose range is `dolby_vision` or that carries
  `delivered_dolby_vision_profile` — "a protocol violation".
- `delivered_range_presentable` (`sharing_start_decode.rs:127-150`) returns
  `false` for any range other than sdr/hdr10/hlg.
- Effective selection is already a pass-through of the Start
  (`http/shared_receiver_successor.rs:386`); it reports DV once the two
  decoders admit it. A directed dynamic-range change is refused at
  `shared_receiver_successor.rs:274`; out of scope (§5).

Clients carry matching dead guards (`preserve_dolby_vision != true`):
`clients/android/.../SharedPlaybackModels.kt:346`,
`SharedDecisionClient.kt:153`, `clients/apple/Sources/SharedDecisionClient.swift:69`,
`SharedPlaybackModels.swift:362`, `web/core/shared-decision.js:109`. Shipped
clients never send `true` for shared, so they do not block M1.

### 1.5 Version skew

Source and receiver are independent installs. An old B fails closed on a DV
Start (§1.4), so a new Source must not deliver DV to a receiver that has not
said it can relay it. The precedent is the bounded-range affirmation:
`RECEIVER_HEADER` (`http/shared_source_playback/receiver_presentation.rs:11-12`,
`:68-77`), sent at `sharing_playback_client.rs:388`, `:778`, `:1064`, `:1859`
and `sharing_client.rs:704`, forwarded through
`authenticated_receiver_presentation` (`http/shared_source_forwarding.rs:253`,
`:270`).

## 2. Target behaviour — same planner decision as local, for every shape

```
 shared Start ─▶ Source: shared planner, player's caps
                    │
     receiver affirmed dolby-vision? ── no ──▶ plan with DV removed from caps
                    │ yes                       (planner picks HDR10 base /
                    ▼                            HDR10-E / tone-map itself)
        the plan local would make
                    │
        build it under Source custody ─▶ HLS through B, facts intact
```

| Shape | Local today | Shared after this effort | Milestone |
|---|---|---|---|
| DV P5 / P8 preserve (copy) | yes | same | M1 |
| P7 → P8.1 convert (copy) | yes | same | M1 |
| DV → HDR10 base strip (copy) | yes | same bytes and index identity as local | M1 |
| HDR10 / HLG copy | yes | unchanged; master carries range facts | M1 (M2 for master) |
| Native subtitles over HDR10 / HLG / DV | yes | same | M1 |
| HDR10-E encode, DV compatible-base encode, HDR→SDR | yes | same | M1 |
| P5 encode (RPU render to SDR / HDR10) | yes | same, proof owned by the Source | M3 |
| Processed DV / FEL re-encode (post-first-frame upgrade) | yes | same, after a design task | M4 |

Next-best rule: when a shape cannot be delivered (old receiver, or P5 encode
before M3), the Source removes the unsupported capability from the caps it
plans with and lets the planner choose. It never returns
`sharing_start_dolby_vision_unsupported` after M3. No feature gate and no
Developer toggle is added; nothing here needs an enable.

## 3. Non-goals

- **No B-side re-planning.** B validates and relays; it never derives a
  decision (`sharing_start_decode.rs:124-126`). Fallback is the Source's plan.
- **No new watchdog, retry loop or shared-only producer.** Every change routes
  an existing shared code path (`copy_normalization`, `index_pass`,
  `DoviProofs`, the master generator) through Source custody.
- **No change to pairing, grants, pinned TLS or the Start wire shape** beyond
  the vocabulary in §1.4 and the header in T2.1.
- **Directed dynamic-range changes and shared direct play of DV** stay as they
  are (§5).

## 4. Milestones

### M1 — DV and HDR copies, native subtitles and compatible-base encodes play (most titles, soonest)

One stacked set; run the fast lane once on the top head.

**T1.1 — Narrow the delivery policy to its real reason.**
Files: `transcode/source_actor.rs` (`SourceDeliveryRefusal`,
`source_delivery_refusal` `:2148-2197`).
Change: delete the `preserve || convert` arm and the `native && hdr_route`
arm. Replace the DV arm with: refuse `DolbyVision` only when
`source_recipe_is_encoded(request)` **and**
`plurx_core::playback::dolby_vision_needs_rpu_render(file)` (the refusal-free
spelling of `needs_dovi_reshape`, named at `plan.rs:1248-1249`; confirm it
exists and matches). Keep the burn arm unchanged. Update the enum docs.
Call sites (`shared_source_direct.rs:324`, `source_actor.rs:1594`,
`growing.rs:690`) need no edit.
Tests — add to `transcode/tests/source_actor.rs`:
`source_delivery_policy_admits_dolby_vision_preserve_and_convert_copies`,
`source_delivery_policy_admits_native_subtitles_over_hdr10_hlg_and_dolby_vision`,
`source_delivery_policy_admits_compatible_base_dolby_vision_encodes`,
`source_delivery_policy_refuses_only_profile5_encodes_until_the_proof_is_owned`.
Update: `shared_source_playback/tests.rs::sharing_source_http_dolby_vision_refusal_precedes_the_claim_and_owns_no_cleanup`
— `SourceFixtureMode::DolbyVisionEncoded` sets only `hdr='dolby_vision'`
(`shared_source_playback.rs:2940-2947`); make it a Profile 5 row (profile 5, no
compatible-base label) so it still pins the remaining refusal, and rename the
mode `DolbyVisionProfile5Encoded`.
Risk: a compatible-base DV encode now reaches
`prepare_source_vod_encoding`; confirm `encoder_and_grade_preferring`
(`manager/create.rs:3297-3306`) spawns nothing for it (reasoned from
`plan.rs:1165`, not run).

**T1.2 — One `have_dovi`, the boot-measured one.**
Files: `vod/serve/create.rs:2660-2664`, `source_actor/growing.rs:419-424`,
`source_actor.rs:1326-1331` (`prepare_source_cold_index`), `http/stream.rs:2566`.
Change: use `self.dv_strippable()` / `state.system.dovi_rpu` /
`self.shared`'s equivalent (thread the bool into `VodShared` if it is not
there) in all four; delete the "unowned child" comment. In `stream.rs` drop
`local_derivation &&` from `have_dovi` only — the census read keeps its
policy split.
Tests: `transcode/tests/source_actor.rs::source_copy_identity_for_dolby_vision_equals_the_local_index_identity`
(build `CopyVideoOptions` through the Source helper and through
`copy_video_pipeline`, assert equal `fragindex::identity_for` for
strip / preserve / convert, both `dv_strippable` values);
`http/shared_library/tests.rs::shared_decision_reports_a_locally_indexed_dolby_vision_title_as_indexed`.
Prove each rejects the old code by restoring `false` once.
Risk: fixtures built with `dv_strippable: false` (`construct.rs:148`) keep
today's identity; fixtures that set it change identity — expected.

**T1.3 — Copy-head validation understands DV.**
Files: `http/hls/create.rs` (`validate_prepared_copy_head` `:6878-6975`).
Change: add the `dolby_vision` range. Expected transfer comes from the
delivered profile via `plurx_core::playback` (compatible HDR10 base → PQ, HLG
base → HLG; Profile 5 → accept the transfer the head declares and skip the
`present.contains(transfer)` test). Require instead that some
`caps.video[*].dv_profiles` contains the delivered profile
(`delivered_dolby_vision_profile`, 8 for a conversion). Sample entry: `dvh1`
for a non-compatible preserve must be admitted against the client's DV
sample-entry claim, not `hls_hevc_sample_entries`; reuse the predicate local
uses for `caps_v2_dv_transport` (`stream.rs:2590-2600`).
Tests — add beside the existing copy-head tests in `http/hls/tests/`:
`source_copy_head_admits_profile8_preserve_for_a_client_that_enumerates_8`,
`source_copy_head_admits_profile7_converted_to_8_1`,
`source_copy_head_admits_profile5_dvh1_for_a_dolby_vision_client`,
`source_copy_head_refuses_dolby_vision_for_a_client_without_the_profile`.
Risk: Profile 5 transfer values in real heads are unverified (§5).

**T1.4 — The Source may index a DV title.**
Files: `transcode/source_actor.rs:1288-1305` (`prepare_source_cold_index`),
`fragindex.rs:2045-2051` (`start_source_index`).
Change: remove the `is_dolby_vision` early return, the
`preserve || convert → Unsupported` return and the `is_dolby_vision` guard;
pass the request's flags and T1.2's `have_dovi` into
`CopyVideoOptions::from_probe(..).with_dolby_vision_conversion(..)`.
`index_pass` already takes the DV passes (`fragindex.rs:1310-1335`). Keep the
HEVC attestation block; `retains_hevc_parameter_sets` / `permits_on_node`
already branch on the options.
Tests: `transcode/tests/source_actor.rs::source_copy_actor_builds_cold_dolby_vision_strip_index_and_serves_hdr10_bytes`
(clone `source_copy_actor_builds_cold_hdr10_hevc_index_and_serves_hdr10_bytes`,
`:157`; guard on `dovi_rpu` and a DV fixture, SKIP otherwise);
`fragindex/tests.rs::source_index_accepts_a_dolby_vision_converting_pass`.
Update: any test asserting "bounded non-Dolby H264/HEVC copy only".
Risk: a 50 GB remux does not scan inside the 35 s start budget (RCA §2.1);
that is unchanged and the growing path covers it. Do not raise the budget.

**T1.5 — B admits a DV Start.**
Files: `http/sharing_start_decode.rs` (`validate_response` `:208-217`,
`delivered_range_presentable` `:127-150`), `http/shared_receiver_ingress.rs:143-152`.
Change: vocabulary becomes `sdr | hdr10 | hlg | dolby_vision`;
`delivered_dolby_vision_profile` allowed only with `dolby_vision` and only in
`5 | 7 | 8`. `delivered_range_presentable` for `dolby_vision`: true only when
the retained caps have a `dv_profiles` entry containing the delivered profile.
Delete the ingress refusal; the derived plan decides.
Tests — in `sharing_start_decode.rs`: replace
`sharing_start_decoder_closes_dynamic_range_and_refuses_dolby_vision` with
`sharing_start_decoder_closes_dynamic_range_and_binds_the_dolby_vision_profile`;
extend `sharing_start_receiver_accepts_hdr_only_when_the_retained_caps_present_it`
(the `dv` case at `:369-374` flips for caps that enumerate the profile) and add
`sharing_start_receiver_refuses_dolby_vision_the_retained_caps_do_not_enumerate`.
In `shared_receiver_ingress.rs`: `sharing_receiver_ingress_forwards_a_dolby_vision_ask_for_the_source_to_plan`.
Risk: none beyond skew, closed by T2.1; land T2.1 before any deploy.

**T1.6 — End-to-end fixtures.**
Files: `http/shared_source_playback.rs` (`SourceFixtureMode` `:2545-2555`,
fixture `:2725-2950`), `shared_source_playback/tests.rs`.
Add modes `DolbyVisionPreserved` (P8.1 copy), `DolbyVisionConverted`
(P7 → 8.1), `NativeHdr10Copy`, `DolbyVisionCompatibleEncoded`. Tests through
`actual_source_resource_delivery`:
`sharing_source_http_dolby_vision_preserved_h1_h2_and_confirmed_end`
(assert Start range `dolby_vision`, profile 8, init has the DV record),
`sharing_source_http_dolby_vision_converted_h1_h2_and_confirmed_end`,
`sharing_source_http_native_hdr10_copy_h1_h2_vtt_and_held_body_end`
(assert master has `VIDEO-RANGE=PQ`),
`sharing_source_http_dolby_vision_base_encoded_to_sdr_h1_h2_and_confirmed_end`.
A DV fixture needs a real RPU stream; if none can be generated with the
repo's ffmpeg, register these as operator-run fixtures with the checked-in
sample the DV tests under `copyseg` / `fragindex` already use, and SKIP
without it. A generated fixture without inspected output is not evidence of
DV fidelity (`IMPLEMENTATION.md:6311-6314`).

**M1 acceptance:** on a two-server lab pair, a P8.1 title and a P7 title start
on an Apple TV and a Shield as Dolby Vision through B, an HDR10 title with an
English text subtitle starts in HDR10, and a P7 title on an SDR browser starts
tone-mapped. Server-side evidence: Start `delivered_dynamic_range` /
`delivered_dolby_vision_profile` on B and the Source session log.

### M2 — Skew and master facts

**T2.1 — Receiver affirms Dolby Vision relay; the Source plans without DV otherwise.**
Files: `http/shared_source_playback/receiver_presentation.rs`,
`sharing_playback_client.rs:388,778,1064,1859`, `sharing_client.rs:704`,
`http/shared_source_forwarding.rs:253,270`, `http/shared_source_ingress.rs:274`.
Change: new header `x-plurx-receiver-dynamic-range: dolby-vision-v1`, sent and
forwarded exactly as `RECEIVER_HEADER` (add
`authenticated_receiver_dolby_vision()` to `AuthenticationHeaders`).
`normalize_wire` gains the flag: when not affirmed, clear
`caps.video[*].dv_profiles`, `caps.dv_transport` and the display DV claim
before the typed session and canonical recipe are derived, so the planner
picks the HDR10/HLG base strip, HDR10-E or tone-map on its own and the
fingerprint matches what was planned. Add a plan note
(`"receiver does not relay Dolby Vision"`).
Tests — in `receiver_presentation.rs`:
`dolby_vision_caps_survive_only_an_affirming_receiver`,
`unaffirmed_receiver_plans_the_compatible_base_and_never_refuses`; in
`shared_source_forwarding_tests.rs`:
`sharing_source_forwarding_carries_the_dolby_vision_affirmation`; in
`sharing_client.rs` keep
`sharing_source_start_preserves_only_the_authenticated_dv_refusal_code` (an
old Source still sends it).
Risk: `public_recipe_hash` (`receiver_presentation.rs:148-152`) must stay
stable for an old receiver; assert it in the first test.

**T2.2 — Every HDR/DV shared session opens on the master.**
Files: `http/hls/create.rs:6600-6609` (`start_response`),
`source_actor.rs` / `growing.rs:225-275` (Master resource),
`transcode/source_subtitles.rs`.
Why: local Apple playback always asks `nativeSubtitles: true`
(`clients/apple/Sources/PlayerController.swift:4153`) and so always gets a
master; the shared Apple and Android clients ask only when a subtitle is
selected (`SharedPlaybackModels.swift:414`, `SharedPlaybackSession.kt:140`),
so a shared HDR/DV session opens on `index.m3u8` with no `VIDEO-RANGE` or
`SUPPLEMENTAL-CODECS`. Profile 8.1 is `hvc1`; without the master AVPlayer has
nothing that says Dolby Vision.
Change (server-side, no client release): `playlist_url` is the master when
`native_subtitles` **or** the delivered range is not SDR; the Master resource
serves with an empty admitted track set when no native tracks were prepared
(`SourceNativeTracks` must not be required to build it).
Tests: `http/hls/tests/…::source_start_response_opens_hdr_and_dolby_vision_sessions_on_the_master`,
`shared_source_playback/tests.rs::sharing_source_http_dolby_vision_master_declares_supplemental_codecs_without_subtitles`.
Update: `sharing_source_bound_invocation_projects_the_complete_start_response`
(`tests.rs:996`) if its fixture is HDR.
Risk: see §5 Q1 — confirm on one Apple TV before and after.

**T2.3 — Shared clients ask for Dolby Vision as local does; remove the stale copy.**
*(Corrected 2026-11-15: the guards are not dead — §6.1.)* The planner clamps
the derived plan down to the ask, and no shared client sends
`preserve_dolby_vision: true`, so without this task no device gets Dolby
Vision over sharing.
Files: Apple `SharedPlaybackModels.swift` (`SharedPlaybackPlan.make` sends
`preserveDolbyVision` for a copy from the decision's verdict, the rule of
`PlayerController.shouldPreserveDolbyVision`; the init guard goes),
`SharedDecisionClient.swift` and `SharedPlaybackControl.swift` (guards go);
Android `SharedPlaybackSession.kt` (`sharedPlaybackPlan` sends
`preserve_dolby_vision` for a copy from `delivery.preserve_dolby_vision ?:
preserve_dolby_vision`, as `PlayerScreen` / `hlsSessionRequest` do),
`SharedDecisionClient.kt`, `SharedPlaybackModels.kt` (guards go); web
`core/shared-decision.js` (guard and `dolbyVisionUnsupported` go; the
refusal message for an older or Profile 5 Source stays, reworded) and
`player/decode-tiers.js` (`choosePlayRoute`'s Dolby Vision refusal goes;
the copy open already sends local's ask). There is no convert field on any
client: the server's conversion follows the preservation
(`review.convert_dolby_vision &= review.preserve_dolby_vision`). Docs:
`IMPLEMENTATION.md` §"Dolby Vision follows the local plan",
`OPERATIONS.md`, `SHARED-LIBRARIES-STATUS.md`, `HANDOFF.md`.
Tests: `SharedDecisionClientTests.swift`
(`testInitialStartSendsTheDolbyVisionPreservationAsk`, case 4 removed),
`SharedPlaybackControlTests.swift`
(`testSharedBurnAndHDRPlanUseSourceOwnedStartFields` asserts the ask),
`SharedDecisionClientTest.kt`
(`dolbyVisionCapableSharedStartAsksToPreserveExactlyAsLocalDoes`),
`tests/web/shared-decision.test.js`, `tests/web/file-context.test.js`.
Client edits are code: Apple build and Android `versionCode` bumped; they go
out through TestFlight / Play internal.

### M3 — Profile 5 encodes: the Source owns the pixel proof

**T3.1 — `SourceDoviProofOperation`.**
Files: `transcode/source_preparation.rs` (model on `start_source_probe`
`:354`, `SourceProbeOperation` `:320-352`, `run_authorized_child` `:802`),
`ffmpeg.rs:4619-4720` (`dovi_probe_output`, `dovi_reshape_changes_pixels`),
`transcode/dovi_proofs.rs`, `transcode/manager/plan.rs:1159-1228`,
`source_actor.rs:1424-1494` (`prepare_source_encoded_recipe`).
Change: split `dovi_reshape_changes_pixels` so the two probe runs take a held
fence and an executor; the Source runs them through `SourceCommandExecutor`
under its copy permit, its held `open_source_playback_fence`, its deadline and
`cancel`/`settle`, then records the verdict with a new
`DoviProofs::record(key, verdict)` (non-test). `require_dovi_renderer` then
finds the remembered verdict and spawns nothing. `Inconclusive` is not
recorded and maps to the existing retryable not-yet answer (S7), never a
refusal. Join the operation in `SourcePreparationSettlements`
(`source_actor.rs:341-354`) so retirement settles the child.
Then delete `SourceDeliveryRefusal::DolbyVision`,
`SourceStartFailure::DolbyVisionUnsupported` (`shared_source_playback.rs:300`,
`:1222`) and the Source-side emit; keep `PeerError::DolbyVisionUnsupported`
decoding (`sharing_client.rs:1621`) for old Sources.
Tests: `transcode/tests/source_actor.rs::source_profile5_proof_runs_under_source_custody_and_settles_on_revoke`,
`source_profile5_refuted_proof_plans_like_local_and_spawns_no_second_probe`,
`transcode/dovi_proofs.rs::a_recorded_verdict_is_found_without_starting_a_flight`,
`shared_source_playback/tests.rs::sharing_source_http_profile5_encoded_to_sdr_h1_h2_and_confirmed_end`
(scripted proof via `scripted_dovi_pixel_proof`). Replace the T1.1
"refuses only profile5" test.
Risk: two 30 s probes inside a start budget; the Source worker row must show
in Activity as this session's work with Stop (the hardware-attribution rule).
Until T3.1 lands, a P5 encode keeps today's typed refusal — it has no
compatible base, so there is no lower plan to fall to.

### M4 — Processed DV / FEL re-encode over sharing

**T4.1 — Design note first** (a new `SHARED-DV-PROCESSED-PLAN.md` beside this plan,
with its row in `docs/streaming/README.md`), then tasks. Local processed DV
is a post-first-frame upgrade through the M6 prepared replacement; a shared
session stages no successor on the Source
(`web/core/shared-decision.js:93-101`) and `prepare_vod_encoding_with_source`
treats a processed destination as needing a "fresh admitted owner
qualification" the Source path never supplies
(`manager/create.rs:2903-2910`, `:2945-2974`, `dv_runtime::ToolSet::capture`).
The note must settle: Source custody of the `dv_runtime` tools and scratch,
how the processed instance key namespaces per grant
(`vod/serve/create.rs:16`, `source_rendition_key`), and whether the upgrade
rides B's existing successor (`shared_receiver_successor.rs`) or a new Source
Start. Depends on A1/S8–S9 of the playback-start plan. Until then the Source
serves the first-frame delivery (preserve / convert / base) that local also
starts with, which is the planner's own next-best.

## 5. Unknowns

1. **Does AVPlayer present DV 8.1 / HDR10 from a bare media playlist?** The
   code says the master is required (`playlist_text.rs:238-247`, `:275-279`)
   but shared HDR10 has only been tested server-side. T2.2 assumes it is
   required. Check one Apple TV on an HDR10 shared title before T2.2.
2. **Why DV was excluded from the Source cold index.** No reason is recorded
   beyond "existing warm indexes retain their established support policy"
   (`source_actor.rs:1291`). T1.4 assumes it was scope, not a hazard.
3. **Profile 5 copy-head facts** — the transfer value and sample entry a real
   P5 head declares after promotion (T1.3).
4. **A DV test fixture.** Whether the repo can generate an RPU-bearing stream
   or needs a checked-in sample (T1.6).
5. **`dolby_vision_needs_rpu_render`** is cited in a comment
   (`plan.rs:1248`); confirm name and semantics.
6. **Shared direct play of DV** — the Apple client keeps DV off direct
   (`SharedPlaybackModels.swift:374-385`); untouched here.
7. **Old Source, new B.** B still gets
   `sharing_start_dolby_vision_unsupported` and shows it. Default taken: no
   B-side retry; peers upgrade (see
   `docs/features/SHARED-LIBRARIES-COORDINATED-UPGRADE.md`). Paul's call if
   peers he does not control must keep playing DV titles as HDR10 meanwhile.

## 6. Execution log

Branch `claude/shared-dv-hdr-m1` (stacked on `effort/shared-dv-hdr`); no PR
opened yet. Tests run on the pinned gate image (FFmpeg 6.1, no `dovi_rpu`).

| Task | Agent-Model | Agent-Session | PR | State |
|---|---|---|---|---|
| T1.1 | claude-opus-5-5 | session_01BCWLA21tCtxWeNBWRHWsWn | — | built `c9f1c464f`; 4 new tests pass, fail on the old policy |
| T1.2 | claude-opus-5-5 | session_01BCWLA21tCtxWeNBWRHWsWn | — | built `df5fb09f5`; 2 new tests pass, fail with `false` restored |
| T1.3 | claude-opus-5-5 | session_01BCWLA21tCtxWeNBWRHWsWn | — | built `7b3344929`; 4 new tests pass (3 admit tests fail on old code) |
| T1.4 | claude-opus-5-5 | session_01BCWLA21tCtxWeNBWRHWsWn | — | built `022cbadf2`; index test passes; DV strip actor test SKIPs without `dovi_rpu` |
| T1.5 | claude-opus-5-5 | session_01BCWLA21tCtxWeNBWRHWsWn | — | built `60ea3ae03`; 4 new/rewritten tests pass, fail on old decoder/ingress |
| T1.6 | claude-opus-5-5 | session_01BCWLA21tCtxWeNBWRHWsWn | — | built `a7602fc27` (+ ledger `468e9e90d`); 4 e2e fixtures pass |
| T2.1 | claude-opus-5-5 | session_01BCWLA21tCtxWeNBWRHWsWn | — | built `5c4542676`; 3 new tests pass, fail on mutated old behaviour |

| Review | claude-opus-5-5 | session_01BCWLA21tCtxWeNBWRHWsWn | — | built `9ee8d7dcb`..`ad9364440` (findings 1–11); each new test fails on the old behaviour |

Review fixes (2026-11-16): T2.1 no longer adds a relay token — Dolby Vision
is withheld in the effective recipe itself (`normalize_wire`), so an older
owner reads an ordinary recipe and a non-DV recipe is byte-identical; the
shared decision withholds the same way; a non-P5 DV copy head needs a known
base; a Source Start never renders a processed first window. The DV strip
fixtures ran with jellyfin-ffmpeg 8.1 (`dovi_rpu`) on a lab node: strip, conversion,
unaffirmed and the cold strip index pass there. Under FFmpeg 8.1 a Profile
8.1 preserve copy's init carried no `dvvC` and the base encode did not start;
both pass on the gate's FFmpeg 6.1 and are open questions for M2.

### 6.1 Corrections found while building

- **The ask clamps the plan down (§1, §1.4).** `review_client_plan_inner`
  sets a derived `preserve_dolby_vision` to `false` when the client did not
  ask for it. A DV-capable client therefore gets Dolby Vision over sharing
  only when it sends `preserve_dolby_vision: true`; the client guards listed
  in §1.4 are not dead, they withhold Dolby Vision. M1 acceptance on an Apple
  TV or a Shield needs T2.3's client change (or a server-side rule for the
  shared ask), so it moves after T2.3.
- **A fourth closed vocabulary (§1.4).** The Source Start carried the local
  `enhancement` rows, which `StartResponse` never deserializes, so B's
  whole-DTO round-trip refused every Start for a Dolby Vision source (strips
  and base encodes included, before this effort). The Source Start now omits
  them (T1.6); shared clients show no FEL/RPU/DYN rows.
- **T1.1.** `dolby_vision_needs_rpu_render` exists (`plurx-core`
  `playback/mod.rs`) and is Profile 5 only; for an unknown or other
  non-compatible profile it is `false` where `needs_dovi_reshape` errs, so such
  a source reaches the planner's own typed refusal, as local does. The risk
  was checked by reading: for a compatible base `require_dovi_renderer` returns
  `Ok(false)` and `vaapi_hdr10_source_fits` only reads the store; the T1.6
  base-encode fixture runs one.
- **T1.2.** Local serving reads `ffmpeg::has_dovi_rpu()` (a boot-latched
  `OnceCell`) and local indexing `dv_strippable`; the Source branch of
  `vod/serve/create.rs` is the only one `prepare_source_rendition` reaches, so
  the value is threaded in from `dv_strippable`. The shared decision uses
  `state.system.dovi_rpu`, the input of the planner's own strip decision.
- **T1.3.** The copy-head tests live in `http/hls/create.rs`
  (`source_copy_head_tests`), not `http/hls/tests/`. `stream.rs`'s
  `caps_v2_dv_transport` is a log field, not a predicate, and
  `hls_hevc_sample_entries` can only name `hvc1`/`hev1`, so `dvh1` is admitted
  by the delivered-profile claim. The validator also needed the conversion
  flag.
- **T1.4.** The Source pass runs only its owned FFmpeg child; the companion
  Dolby Vision title scan belongs to local `build_attested_pass`. The DV strip
  actor test needs `dovi_rpu` (FFmpeg 7.1+), which the gate image lacks.
- **T1.6.** No checked-in RPU sample exists: the `copyseg` / `fragindex` /
  `stream` Dolby Vision tests inject the real Profile 7 RPU from
  `plurx_core::testfixtures`. The fixtures do the same and inspect the
  delivered record and RPU NAL units. An HEVC copy also needs the VOD side's
  cluster node identity for the cold index's header proof.
- **T2.1.** The affirmation is recorded by the Source ingress in the effective
  recipe as a reserved transport token (`plurx-internal-relay-dolby-vision-v1`),
  which every forwarded owner plans from, so the forwarding wire
  (`shared_source_forwarding.rs` `:253`, `:270`) needs no new field.
  `public_recipe_hash` strips every reserved token, so it is unchanged for an
  old receiver.
