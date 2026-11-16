# Shared processed Dolby Vision / FEL re-encode — design note (T4.1)

**Status:** design, ready for Paul's two calls (§6) · **Effort:** `effort/shared-dv-hdr`, M4 ·
**Written:** 2026-11-15 · **Read against:** `wt-dvhdr-m2` @ `468e9e90d` (M1 code);
`origin/main` @ `29b35e713`. Read, not compiled, nothing run.
Destination in the repo: `docs/streaming/SHARED-DV-PROCESSED-PLAN.md` plus its row in
`docs/streaming/README.md` (T4.1 of `SHARED-LIBRARY-DV-HDR-PLAN.md`).

Paths are relative to `crates/plurxd/src/` unless they start with `crates/` or `docs/`.
Line numbers are from that worktree as it stood, which had uncommitted M2 edits
(`http/hls/create.rs` among them); re-find by symbol. The receiver files cited
(`shared_receiver_playback.rs`, `shared_receiver_successor.rs`,
`crates/plurx-core/src/sharing_resources.rs`) are byte-identical to `origin/main`.

## 1. Answer in five lines

1. **Custody:** the trial and its first window become one more Source preparation
   operation, under the owner's existing permit, playback fence, deadline and
   settlement. One call site changes: the first window stops minting its own
   producer slot and cancel token (`transcode/manager/create.rs:3811`).
2. **Instance key:** unchanged function. The canonical key under it is already the
   Source namespace; the incarnation is the Source assignment's. Reuse is scoped by
   `share:<grant>:<viewer>`. No grant is added to the hash.
3. **Transport:** the upgrade rides **B's existing prepared successor**, which is
   an ordinary second Source Start. No Source-staged successor, no new Start kind.
4. **A1/S8–S9:** none of them blocks M4. The S8 part M4 uses is on main; S9 and A1
   are not on main and are not needed.
5. **A defect in M1 as built** (§5): a Source with HDR processing on renders a
   processed window before the first frame, unowned. Fix before M1 deploys.

## 2. What the code does today

| # | Fact | Anchor |
|---|---|---|
| F1 | Local upgrade: create runs request-only gates and registers an intent on the owner; a first-frame proof takes it and runs one 8 s trial; the verified window is staged as an M6 prepared successor. | `http/hls/create.rs:4098-4118`, `:5137-5150`; `http/hls/processed_upgrade.rs:1-23`, `:438-473`, `:531-560`; `transcode.rs:618` |
| F2 | The gates and the trial read everything from `request.candidate_context` (caps, selected height and grade, catalog query). With none, the gates answer `Ok(None)` silently. | `transcode/manager/create.rs:2225-2232`, `:2354-2361` |
| F3 | A Source request gets a candidate context only when the Source has display-aware Auto on and the body is `quality_auto` and not `copy`. A DV copy Start has none. | `http/hls/create.rs:2105-2148`, `:2412`, `:6605` |
| F4 | `prepare_vod_encoding_with_source` already takes the Source binding: it opens the playback fence and checks the held probe against it. It refuses a processed destination unless `prepared_admission` is passed, and the Source wrapper passes `None`. | `transcode/manager/create.rs:2803-2835`, `:2904-2910`, `:3031-3047` |
| F5 | The first processed window runs on `Arc::new(ProducerSlot::new())` and `CancellationToken::new()`. Only `processing_deadline` bounds it; nothing outside can cancel or settle it. Local survives this because the upgrade races the future against the playback slot's token. | `transcode/manager/create.rs:3803-3813`; `http/hls/processed_upgrade.rs:479-485` |
| F6 | The slots are node-wide and shared by every caller: one processed preparation, four processed instances, and the GPU + CPU admission. | `transcode/manager/construct.rs:98`, `:101`; `transcode/manager/create.rs:2366-2370`, `:2495-2499`, `:3782` |
| F7 | A Source encode prepares under `admit_source_copy`, `open_source_playback_fence`, `gate.current_preparation` before and after, and settles through `SourcePreparationSettlements`. | `transcode/source_actor.rs:1453-1458`, `:1493`, `:341-354`, `:1909-1921` |
| F8 | A Source cannot stage a successor of its own session: every owned-route guard refuses a `media_session_preparations` row naming its incarnation. B's successor is a second ordinary B session with its own Source Start, prepared only on B. | `http/shared_receiver_successor.rs:3-20`, `:409-448`, `:551-585` |
| F9 | B dispatches a successor only when the client's desired selection changes, and withdraws it when the client leaves that ask. | `http/shared_receiver_successor.rs:475-545`; `http/shared_receiver_control.rs:325-337` |
| F10 | B already relays the offer as `ControlAction::Prepare`, and clients already declare `SHARED_PREPARE_REPLACEMENT_ACTION`. | `http/shared_receiver_successor.rs:42-45`, `:617-660` |
| F11 | A mid-film successor already works with a zero origin: the position travels in the request's `start`. | `http/shared_receiver_successor.rs:299-301`, `:430`; `http/shared_receiver_successor_tests.rs:131` |
| F12 | A Source Start cannot carry `enhancement` rows: B's closed decoder refuses them. The delivered range and profile are what cross. | `http/hls/create.rs:6620-6628` |
| F13 | Storage key of a processed instance = `processed_instance_key(canonical_key, request_id, nonce)`; for a Source binding `canonical_key` is `source_rendition_key` (server, catalogue epoch, library, item, file, revision, recipe). | `vod/serve/create.rs:14-22`, `:492-502`, `:2940-2943`; `vod/plan.rs:58-77` |
| F14 | A sharing principal's supersession scope is `["principal","share:<grant>:<viewer>"]`. | `transcode/session_request.rs:476-486`; `crates/plurx-core/src/playback_principal.rs:64-70` |

## 3. Decisions

### 3.1 Source custody of tools, scratch and children

One new operation, `SourceProcessedOperation`, modelled on `SourceProbeOperation`
(`transcode/source_preparation.rs:320-363`). It is the only caller of the trial on a
Source and holds, in order:

| Resource | Held as | Released |
|---|---|---|
| Authority | `owner.gate.current_preparation(&owner.assignment)` before the trial and again before attach (as `source_actor.rs:1458`, `:1493`). Nothing here reads quorum or serving authority itself. | n/a |
| Source bytes | the `open_source_playback_fence` the encode already opens (`create.rs:3031-3035`); the window reads through `SourceInput::open_current_held(&runtime.source.handle)` (`create.rs:3806`), never a path. | with the `RuntimeRecipe` |
| Tools | `ToolSet::capture()` after admission (`dv_runtime.rs:351`); a node fact, hashed, re-checked by `is_current` (`:479`). Nothing from B selects a tool or a route. | n/a |
| Permits | the copy permit for the probe (today), then the node-wide processed preparation slot, instance allowance and `admit_processed_trial` (F6). Same semaphores local uses; no shared-only budget. | preparation slot at trial end; instance allowance at physical cleanup (`vod/session.rs:18`) |
| Deadline | `min(Start deadline, now + PROCESSED_UPGRADE_TRIAL_BUDGET)`, passed as the `prepared_admission` deadline (`create.rs:2374`, `:3780`). | n/a |
| Children | renderer helper and encoder, spawned by `dv_segment::spawn_streaming` (`dv_runtime.rs:1552`) on the **operation's** producer slot and cancel token, replacing F5's fresh ones. | reaped inside `run_private_window` (`dv_runtime.rs:1540-1596`); the operation's receipt joins `SourcePreparationSettlements` |
| Scratch | `shader_cache` TempDir under `runtime_cache` (`create.rs:3771-3772`) and the instance directory under F13's key. | TempDir with the recipe; instance by the existing reaper (`vod/tests/chunk_03.rs:4`, `:47`) |

After attach, later windows run in the rendition's producer and settle through
`SourceProducerAssociationsSettled` like any Source encode (`source_actor.rs:356-362`).
Retirement (End, revoke, lease expiry) cancels the operation and waits its receipt in
the block that already settles index and probe work (`source_actor.rs:1909-1921`).
No watchdog and no timer beyond the deadline above.

### 3.2 Instance key

Keep `processed_instance_key` as is. Set the processed request's `request_id` to the
Source session's incarnation (the value activated at `source_actor.rs:1749`), so the
key is: Source namespace → recipe → this Start's incarnation → nonce. That is one
directory per admitted Start, which is narrower than per grant; two receivers, or two
viewers of one grant, can never share or collide.

Reuse of a live processed root (T4.5) is looked up by
`principal_supersession_scope` (F14) plus the processed hint scope
(`transcode/manager/candidates.rs:519-544`), so it never crosses a grant or a viewer.
A grant id is deliberately **not** hashed into the key: it would duplicate the
assignment and make a re-issued grant orphan live storage.

### 3.3 The upgrade rides B's successor

Chosen: B's prepared successor, started for a new reason. Rejected: a Source-staged
successor (F8 makes it impossible without loosening every owned-route guard) and a
new Start kind (a second start path, against "one path", with its own slot counting
and replay rules that `shared_receiver_successor.rs:9-12` already has).

Flow, mirroring F1 step for step:

1. **Source Start (ordinary).** `prepare_source_playback` runs the request-only gates
   on the snapshot it holds and, when they answer a destination, the Start response
   carries `processed_upgrade: "offered"`. No work is done.
2. **B first frame.** B is the server the client reports to. On the first
   Source-accepted exchange after the offer, with the slot free, the same client
   conditions as today (`dual_player`, `accepts_shared_successor`, the handoff
   setting) and an unchanged ask, B stages a successor whose request is the original
   plus `processed_upgrade: true`. B relays an offer the Source made; it derives no
   plan (`sharing_start_decode.rs:124-126` stands).
3. **Source Start (processed).** The Source re-runs the gates on fresh inputs, runs
   §3.1's operation, attaches the root and publishes a VOD Start whose delivered
   range and profile state the result (F12).
4. **Offer and commit.** Unchanged: `compose_preparation` offers, the client commits,
   the predecessor retires through the one retirement owner (F10).
5. **Decline.** Any refusal is a typed decline of the successor Start; B clears the
   slot and does not retry for this session. The ordinary session is untouched.

A viewer's own change still wins: a different ask withdraws the slot (F9), as locally.

Wire: two optional fields (`CreateSession.processed_upgrade`,
`StartResponse.processed_upgrade`), sent only to a peer that affirmed them through
T2.1's receiver header (`http/shared_source_playback/receiver_presentation.rs:18-19`).
Old B never sees the offer; old Source never makes one.

### 3.4 What A1 / S8–S9 must provide — checked in code

| Piece | Needed by M4? | On main? | Evidence |
|---|---|---|---|
| S8a relay returns a retryable Source answer typed, without retiring | yes (a processed producer can run behind the reader) | **yes** | `http/shared_receiver_playback.rs:1434-1436`, `:2513-2523` |
| S8b Start acceptance no longer requires `vod && origin == 0` | no (processed is a whole-title VOD plan at origin 0) | the cited predicate is gone; origin is fixed at 0 | `http/shared_receiver_playback.rs:1793-1811`; `http/sharing_start_decode.rs:167-177` |
| S8c playlist validator admits EVENT and ENDLIST | no | **yes** | `crates/plurx-core/src/sharing_resources.rs:511-516` |
| S9 receiver carries a start position | no (F11) | **no** — forced to 0 and refused otherwise | `http/shared_receiver_ingress.rs:214` on main; `http/shared_receiver_playback.rs:719` |
| A1 one copy engine (S1–S3) | no — the processed rendition is an encode through the one encode preparation (F4); the first frame is M1's copy | **no** — `prepare_source_cold_index` and the separate growing backing still exist | `transcode/source_actor.rs:1303`; `transcode/source_actor/growing.rs:1-40` |

So the plan's "Depends on A1/S8–S9" is stale. M4's real prerequisites are inside this
effort: M1 (first-frame delivery), T2.1 (the affirmation header), and T3.1's custody
pattern, which T4.3 copies. When A1 lands, nothing here moves: S1 folds the copy
preparations, not the encode one.

## 4. Tasks

Each is one PR onto the effort branch. Fast lane once on the top head.

**T4.2 — One shared session upgrades to processed output (end to end, smallest).**
Files: `transcode/manager/create.rs` (`processed_upgrade_gates` `:2187`,
`prepare_processed_upgrade` `:2318`, `prepare_source_vod_encoding` `:2803`, the first
window `:3797-3813`); `http/hls/create.rs` (`CreateSession` `:200`,
`prepare_source_playback` `:6698`, `start_response` `:6620`);
`http/hls/session_guard.rs:22` (`StartResponse`); `transcode/source_actor.rs`
(`prepare_source_encoded_recipe` `:1446`); `http/sharing_start_decode.rs`
(`validate_response` `:166`); `http/shared_receiver_successor.rs` (`successor_request`
`:268`, `observe_ask` `:475`, `stage_successor` `:551`);
`http/shared_receiver_control.rs:325-337`.
Change:
(a) Lift what the gates and the trial read from `candidate_context` into a
`ProcessedUpgradeAsk { caps, target_height, hdr10_grade, names_processed, fel_qualification, catalog_query }`
with two constructors: from a candidate context (local, behaviour unchanged) and from
a `PreparedSourcePlayback` (`copy_head_caps`, the resolved height and grade). One
gate function, one trial, two callers.
(b) The first window takes its producer slot, cancel token and deadline from the
caller. Local passes what it has today; the Source passes its owner's.
(c) `prepare_source_processed_encoding`: calls `prepare_vod_encoding_with_source`
with `Some((evidence, proof))` **and** `Some((&mut credit, deadline))`, builds the
`PreparedRootAdmission` exactly as `:2495-2523` does, with `request_id` per §3.2.
Called from `prepare_source_encoded_recipe` when the prepared request has a
processed destination.
(d) The two wire fields and their affirmation (§3.3); `validate_response` admits
`processed_upgrade` only as `"offered"`.
(e) B: `HandoffState` records the offer; `observe_ask` gains one arm — ask unchanged,
offer present, not yet dispatched, slot free — returning the current digest;
`successor_request` gains the flag. A refused upgrade Start marks the offer spent.
Tests:
`transcode/tests/source_actor.rs::source_processed_upgrade_builds_under_the_owner_fence_permit_and_deadline`
(scripted `ToolSet::lifecycle_fixture`, `dv_runtime.rs:98`; SKIP off Linux);
`transcode/manager/candidates.rs::processed_upgrade_ask_from_a_candidate_context_matches_todays_gates`;
`http/shared_source_playback/tests.rs::sharing_source_http_offers_a_processed_upgrade_only_to_an_affirming_receiver`,
`::sharing_source_http_processed_start_publishes_profile8_vod_and_confirmed_end`;
`http/sharing_start_decode.rs::sharing_start_decoder_binds_the_processed_upgrade_offer`;
`http/shared_receiver_successor_tests.rs::sharing_receiver_stages_a_processed_successor_once_after_the_first_accepted_exchange`,
`::sharing_receiver_processed_successor_is_withdrawn_by_a_directed_change`,
`::sharing_receiver_refused_processed_start_is_not_retried`.
Update: `transcode/session_request.rs::serialized_processed_destination_cannot_enter_an_ordinary_producer`
(`:1508`, still true for a request without the admission);
`sharing_receiver_observe_ask_dispatches_once_and_withdraws_a_left_ask` (`:652`).
Prove (b) rejects the old code by restoring `CancellationToken::new()` once.
Risks: (1) the successor begins where the viewer was when B asked, and the trial
takes up to 8 s plus the relay; local bounds this with a 4 s head start
(`processed_upgrade.rs:627-641`). The Source should pick the entry at `start` plus
its own lead and state it in `start_seconds`; measure before fixing a number.
(2) What a client does with `preparation: none` when it asked for nothing — expected
nothing; confirm on Apple and Android. (3) `Profile81` needs the Source's own FEL
verdict (`create.rs:2468-2484`); T4.2's fixture covers HDR10-E, T4.5 covers FEL.

**T4.3 — The trial is a settled Source operation.**
Files: `transcode/source_preparation.rs` (new `SourceProcessedOperation`, beside
`SourceProbeOperation` `:320`, using `run_authorized_child`'s fence re-open `:802-818`
as the model); `transcode/source_actor.rs` (`SourcePreparationSettlements` `:341-354`,
the settle block `:1909-1921`).
Change: T4.2's call moves into the operation; its receipt is a third field of
`SourcePreparationSettlements` and `matches` checks it. Retirement cancels and waits
it. A cancelled trial records no failure episode (`dv_runtime.rs:28-48` already
separates cancel from failure).
Tests: `transcode/tests/source_actor.rs::source_processed_trial_settles_its_children_on_revoke`,
`::source_processed_trial_past_its_deadline_releases_the_preparation_slot`,
`::source_processed_settlement_of_another_assignment_is_a_mismatch`.
Risk: the preparation slot is one per node (F6); a stuck receipt would block local
upgrades too. The deadline, not a watchdog, is the bound — assert the slot count in
the second test.

**T4.4 — Attribution and Stop.**
Files: the Source worker's Activity row (not located in this read — find where a
Source encode is listed before starting; `transcode/source_status.rs` is the first
place to look), `http/hls/processed_upgrade.rs:112-150` (`ProcessingSkip`, the wire
local uses to report why an upgrade did not run).
Change: while a Source processed trial or rendition runs, the Source's Activity shows
it as this shared session's work: title, grant and viewer, "processed Dolby Vision
upgrade", why (the destination and the setting that selected it), and Stop. Stop
retires that Source session through the existing admin stop
(`transcode/manager/control.rs:1058`); B then sees an ordinary Source End. On B, the
session row names the Source as the place the work runs. A declined upgrade reports
its reason on B through a relayable field, not `enhancement` (F12).
Tests: `http/tests.rs::activity_lists_a_source_processed_upgrade_with_its_grant_and_stop`,
`transcode/tests/source_actor.rs::admin_stop_of_a_source_processed_session_settles_and_ends_the_start`.
Must land before any deploy of T4.2 (hardware-attribution rule).
Risk: none in behaviour; scope depends on what row exists.

**T4.5 — FEL re-encode and root reuse.**
Files: `transcode/manager/create.rs:2432-2490` (reuse and `reconstruction_qualified`),
`vod/serve/delivery.rs:7-25` (`capture_processed_root`).
Change: reuse needs a predecessor, and a shared Start strips `previous_session_id`
(`shared_receiver_successor.rs:279-298`); look the root up by the Source's own
record of the same principal's live processed session instead (§3.2 scope). FEL:
the Source's title verdict decides, as locally; a pending verdict is the existing
typed not-yet, never a refusal.
Tests: `transcode/manager/processed_reuse_tests.rs::source_processed_root_is_reused_only_within_one_grant_and_viewer`,
`http/shared_source_playback/tests.rs::sharing_source_http_fel_reencode_start_waits_for_a_pending_verdict_as_not_yet`.
Risk: reuse across a B reopen changes the incarnation; the nonce must be new.

**T4.6 — Qualification.** D1's bar, through the relay: 20 measured switches per
device and output path with frame and audio evidence
(`docs/streaming/PLAYBACK-START-PLAN.md`, lane D1). Physical devices: a GPT prompt,
written when T4.2–T4.4 are on the effort branch. Then retire plan §2's "after a
design task" row and `web/core/shared-decision.js:93-95`'s comment.

## 5. M1 correction — not an M4 task

After T1.1 a compatible-base DV encode reaches `prepare_source_vod_encoding`. With
`DV_HDR_PROCESSING` on at the Source, a Profile 8.1 or 7 source and an HDR10 encode
ask, `vod_dv_processing_from_snapshot` answers `AwaitingFirstWindow`
(`create.rs:1391-1410`; `crates/plurx-core/src/transcode/dv_processing.rs:1982-2023`),
and because a Source request has no candidate context (F3) the legacy arm captures
the tools and renders a processed window **inside the Start**
(`create.rs:2948-2956`, `:3653`), on F5's unowned slot. That is processed work before
the first frame (ruling 3) outside Source custody. Reasoned from the code, not run.
Fix: that arm applies only when `source_evidence.is_none()`; a Source reaches
processing only through T4.2. Test:
`transcode/tests/source_actor.rs::source_compatible_base_encode_never_renders_a_processed_window_in_the_start`.
T1.1's risk note covers `encoder_and_grade_preferring` only.

## 6. Open — Paul's calls and unknowns

1. **Paul:** shared viewers compete for the Source's one processed preparation slot
   and its GPU on equal terms with local viewers (F6). Default taken: equal, first
   come, the Source's own `DV_HDR_PROCESSING` / `DV_FEL_REENCODE` settings decide and
   B's do not. Say so if the Source's local viewers should come first.
2. **Paul:** B's existing "prepared quality handoff" setting
   (`shared_receiver_control.rs:326-333`) also turns the upgrade off on B, since it is
   the same successor. Default taken: yes, one switch for one mechanism.
3. Unknown: the Source Activity row (T4.4).
4. Unknown: whether the renderer children die when the trial future is dropped; T4.3
   does not rely on it.
5. Unknown: the right start lead (T4.2 risk 1).

## 7. Execution log

| Task | Agent-Model | Agent-Session | PR | State |
|---|---|---|---|---|
| T4.1 design note | claude-fable-5-2 (sub-agent) | 01BCWLA21tCtxWeNBWRHWsWn | — | written 2026-11-15; not yet in the repo; awaiting §6 calls |
| M1 correction (§5) | | | | open — flag to the M1 owner |
| T4.2 | | | | |
| T4.3 | | | | |
| T4.4 | | | | |
| T4.5 | | | | |
| T4.6 | | | | |
