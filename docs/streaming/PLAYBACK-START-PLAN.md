# Playback start — implementation plan

**Status:** open — build plan for [CINEMA-END-TO-END-REVIEW-2026-11-07.md](../reviews/CINEMA-END-TO-END-REVIEW-2026-11-07.md). Rulings taken 2026-11-07 (§0). This file is the ONE shared status for the effort: claim a lane by editing its row (§6) in a draft PR and record the execution log (§7).
**Routing:** this plan → adversarial review (Fable) and Astra; the lanes in §3 → build agents (Opus; Sonnet where marked). Nothing here is gated in code: enable rows go in Settings → Developer as advisory readiness, never blocking.
**Branch:** `effort/playback-start`. Lane PRs land in the effort branch (draft → one adversarial review per lane deliverable → fix → fast lane once on the lane head); effort → main after one unit run, merged by the lane owner. Paul deploys.
**Author:** Claude (claude-fable-5-1, session 01BCWLA2SJPLAmibeaz9L5xb).

## 0. Rulings (Paul, 2026-11-07)

1. Rolling-live copy and indexed-VOD copy become **one engine**; the first play produces the index.
2. The fragment index is **keyed on the source and an output contract**, not on the engine binary; per-title fallback when serve-time verification fails.
3. Processed Dolby Vision / HDR10-E is an **upgrade after the first frame**, never work before it; the upgrade must be seamless (player-native variant switch, zero dropped frames at the seam) before it leaves the Developer tab.
4. Learners and idle voters **may index**: lab4 and lab6 preferred, media1 with yield, lab3 (learner) lowest priority.

## 1. Target contract (what PLAYBACK-START.md will say when the effort closes)

- A start is one client clock. The client sends `x-plurx-startup-remaining-ms`; the server answers within it with exactly one of **ready** (playlist URL) or **not yet** (typed code, `Retry-After`, the server's own estimate). No ladders, no codeless refusals.
- Create is one snapshot read → one pure decision → one durable write → spawn → answer. Nothing advisory or speculative runs before the answer. Create p99 < 1 s on the fleet.
- Every engine publishes its first playlist at **3 complete segments or ENDLIST**. Copy produces faster than real time, so a copy start is bounded by I/O, not by a media-duration gate.
- The fragment index is a by-product of the first play of a title and survives a deploy. The background queue only indexes titles nobody has played.
- Settings, routes and catalogue are node-local views of the applied raft log; only fences are consistent reads.
- Every start is attributable in Activity → Starts: route, the refusal that chose it, phase timings, owner node, the title's index state.

## 2. Traceability — every review finding has a lane

| Review item | Lane |
|---|---|
| §1.1/§5.1 index queue starved (`pretranscode_worker_idle` gate, 2 I/O slots, learners excluded, `pipeline_version` filter, 16 claims/2.7 d) | C3 (wedged at the 4,096 cap by retired-digest rows — verified in the replicated state), C1 |
| §2 verdict "idle" while 3.9k wait / 7.7k failed (two tables) | C2 |
| §2 7.7k failed with unknown `last_error_code` | C2 (surface), C3 (requeue) |
| §1.2/§5.3 rolling first publication = 16 s of media | A1, A2 |
| §1.3/§4 startup repairs live in the slow path; stale "not deployed" lines | A1 retires the path; H1 fixes the lines |
| §1.4/§5.4 create: 10 phases, 3–4 snapshot reads, held-source ffprobe per start, catalog fan-out, link receipts, network prior, 10 s budget unknown to clients, detached worker | D2, D3 |
| §3.1/§4 `prepare_processed_hdr10` renders the first window inside create; pins owner to ingress; holds the source fence | D1 |
| §4 caps v2 catalog: ~80 authority reads, 2 s deadline, second pass, 16-row cap rejects Android caps | D3 (precompute); D2b (16-row cap, Phase 1) |
| §4 recipe tokens / ffmpeg deps retire every index and processed root per deploy | B1 (indexes); processed roots stay recipe-keyed by design — noted in B1 |
| §4 live presentation 410 vs ruling "either path first class" | A3 |
| §4 library worker driver does not reach index claims | C1 |
| §1.5/§5.5 consensus for everything (168k playback, 82k settings authority reads; 1 s route cache) | E1 |
| §3.3/§5.6 eight clocks, three retry owners, capacity refusal codeless, terminal lists differ | D2 (server half, Phase 1), F1 (clients) |
| §2 `authority_fence won` 16 of 18 rolling sessions | R1 (investigated: client release is implemented as a fence projection) |
| §2 mixed-version fleet (lab4 one build behind), `PROTOCOL_VERSION` exact placement | H2 |
| §5.7 no product attribution of a start | G1 |
| §4 process: 22 overlapping startup docs, four gate values, no owner | A3 (supersede), H1 |
| §3.2 segment GET waits 8 s vs hls.js TTFB 10 s; segment 20 s inside 35 s request; playlist 55 s from mirrors | F1 |

## 3. Lanes

Each lane: root cause → change → anchors → tests → acceptance → size/tier. Tests must pin the call site and be shown to reject the old code (reinstate the old behaviour behind the same API once and watch the test fail). Fleet acceptance readings are taken from `/metrics` on media1/lab4/lab6 before and after Paul's deploy; the lane is not done until the after-reading is in §7.

### Phase 1 — repair in place (days)

**C1 — Indexer admission floor.** Root cause: `work_cluster_fragment_index_queue` → `resolve_analysis_requests` claims with `viewer_only = !pretranscode_worker_idle()` (`crates/plurxd/src/state.rs:9415-9450`; predicate `transcode/manager/construct.rs:1653`), so a node with any encoder in use claims only playback-waited requests; `MAX_REQUESTS_PER_PASS = 2`; learners get only targeted rows (`claim_analysis_request_for_capacity`, `crates/plurx-core/src/store/sqlite/fragment_index_cluster.rs:821-944`, `{untargeted}` = `0` when `targeted_only`); the capacity clause caps the storage domain at two readers. Change: (a) `viewer_only` only while `admissions.live_is_waiting()` — an encoder *in use* is not a reason to stop indexing; pace with the existing `pacing.delay(progressed)`; (b) learners claim untargeted `fragment_index` rows when `may_execute_job(FragmentIndexBuild)` is true, after voters' forced/foreground rows (add a priority term: learner claims only rows older than `ANALYSIS_FORCED_PRIORITY_BOOST_MS`); (c) per-pass count = free local analysis capacity, not 2; (d) Developer tab advisory row "Indexing while serving" showing live waiters, in-use encoders, domain slots. Ruling 4 node preference is expressed by the existing `target_node_id` for admin requests only; untargeted rows go to whoever is idle. Tests: SQL-level claim tests for (a)–(c) (`crates/plurx-core/src/store/sqlite/fragment_index_cluster.rs` tests); a `state.rs` test that a node with `in_use() > 0` and no live waiter still claims. Acceptance: `plurx_analysis_queue_claims_since_start` increases on every voter and on lab3 within an hour of deploy; `retry_wait` + `failed` depth falls day over day. Size M, Opus.

**C2 — Honest verdict and visibility.** Root cause: `analysis_queue_verdict` inputs come from `cluster_fragment_index_jobs` (`crates/plurx-core/src/store/hiqlite.rs:6036-6052`) while the depth gauge unions `analysis_requests` (`:5985-6010`); the verdict reads `idle` with 3.9k requests waiting. Change: compute `claimable`, `claimed_24h`, `ready_24h`, `attempt_limit_24h` over the same union the depth gauge uses (both stores, SQLite twin too); add `plurx_analysis_queue_failed_by_code{code}` from `last_error_code`; Settings → Activity shows failed-by-code, oldest retry-wait age, claimable count, verdict. Tests: `queue_verdict_tests` case "3,000 queued past not_before, zero claims → dead"; a store test that the verdict and depth read the same rows. Acceptance: on the current fleet data the verdict reads `dead` or `degraded`, never `idle`. Size S, Sonnet.

**C3 — The queue is wedged at its cap by retired-version rows (do this first).** Read from media1's replicated state on 2026-11-07: 4,097 active rows against `MAX_ACTIVE_ANALYSIS_REQUESTS = 4_096` (`crates/plurx-core/src/store/fragment_index_cluster.rs:1780`) — 3,878 `fragment_index` rows `queued` with `attempts = 0`, all past `not_before_ms`, all under two engine digests (`f39da144…` 2,561, `85e6fa5e…` 1,317) that no running node has since the 2026-11-04 deploy (the last `ready` under `85e6fa5e…` is 2026-11-03 23:12; six digests since 2026-08-26). Root cause chain: the claim filter `pipeline_version = ?5` (`sqlite/fragment_index_cluster.rs:903`) makes them unclaimable; the sweep fails only rows with `attempts >= max_attempts` (`:888-894`), so 0-attempt rows live forever; `enqueue_analysis_request` refuses when active rows ≥ the cap except 256 playback headroom (`:631-634`), so discovery cannot enqueue current-digest requests; a node that does reach a stale row fails it `pipeline_version_unavailable` (`state.rs:10327`). Net: after any deploy that changes the engine digest, the backlog fills the cap and indexing stops fleet-wide until someone clears it — which is what the fleet has been doing since Nov 4. A migration already exists — `POST /api/v1/analysis/reconcile` (`http/analysis_reconcile.rs`, `store/analysis_reconcile.sql`) replaces a queued row with one under the current digest — but it is an admin page that applies 100 explicit candidates per call (39 clicks for today's backlog) and nobody runs it after a deploy. Change: (a) run that reconciliation automatically from `work_cluster_fragment_index_queue` whenever `fragment_index_engine_is_current()` and queued `fragment_index` rows carry a digest no live node reports (the cluster snapshot carries each node's digest; add it to `MediaNodeSnapshot` if absent), bounded per pass, both stores; (b) the cap counts only claimable rows (current digest or untargeted) so unclaimable rows can never wedge it; (c) `POST /api/v1/analysis/requeue` with a `last_error_code` filter and a dry-run count, surfaced in Activity, for the 2,468 `attempt_limit` / 723 `index_process_failed` / 768 `index_completion_unverified` jobs once C1 gives them a chance. Bridge until B1 removes the engine from the key. Tests: settle test with two digests and a wedged cap; enqueue succeeds after the sweep; API requeue filter/dry-run. Acceptance: within one discovery cadence of deploy, `queued` rows carry the current digest, `claims_since_start` rises, and `ready_24h > 0`. Size M, Opus.

**D1 — Processed DV/HDR10-E becomes an in-session upgrade.** Root cause: `prepare_processed_hdr10` (`transcode/manager/create.rs:1929-2303`, called `http/hls/create.rs:3713-3866`) renders and encodes the first processed window inside create under `create_enhancement_deadline(8 s)`, re-reads the planning snapshot, pins the owner to the ingress and holds the source fence. Change: create resolves the ordinary plan only. When the ordinary rendition is publishable, the owner node runs the existing processed-trial path (`prepare_vod_encoding_with_source` processed branch, `processed_preparation_slot`, `processed_instance_slots`) as a second rendition of the *same* session: the session is opened as a continuous family (`create_continuous`, `ContinuousCandidatesRequest`) whose master advertises the ordinary variant immediately and adds the processed variant (`VIDEO-RANGE`, `SUPPLEMENTAL-CODECS` dvh1/HDR10-E as the recipe dictates) when its first window verifies; the control exchange announces it (`delivery.preparation = offered`) and the client switches at a segment boundary with its player's variant switch (AVPlayer, Media3, Safari native). Chromium cannot play DV — no change on hls.js. The prepared-replacement handoff (M6) stays as the fallback for players that will not switch variants in place. Developer tab: "Processed DV upgrade" readiness rows (seam test passed per platform, renderer present, slot free) — advisory. Tests: create phase ledger contains no `processed_qualification` phase (assert on the rendered phases string); a continuous-family test that a late-added variant appears in the master without changing the ordinary variant's URIs; the existing processed-candidate contract tests retargeted to the post-start path. Acceptance: DV-title TTFF equals non-DV TTFF on the fleet; seam: 20 consecutive upgrades per platform (Apple TV, Android TV, Safari) with zero dropped frames ±2 s and no audible seam, measured with the frame-drop recorder (`docs/clients/FRAME-DROP-RECORDER.md`). Size L, Opus; Fable review.

**D2 — Server half of the start contract.** Root cause: create budget defaults to 10 s because no client sends `x-plurx-startup-remaining-ms` (`http/hls/create.rs:2931-2935`); a capacity refusal is a codeless 503 (`session_start_error`, `:5914+`, `RETRYABLE_CAPACITY_PREFIX`) that no client retries while timeouts are retried; the detached worker past `placement_deadline` (`:4158-4183`) spends a slot the viewer never gets. Change: (a) clients send the header from their one start clock (web `beginPlaybackPreparation`, `player/measurements.js:71`; Apple `PlayerControllerSupport.swift:2659`; Android `PlaybackPolicy.kt:200`) — the client half of F1 lands here because the server half is useless without it; (b) capacity refusal → `transcode_capacity_pending` with `Retry-After` from the admission estimate (it is already in `START_NOT_YET_CODES`); (c) on placement timeout the create keeps the claim and answers `startup_timeout` with `Retry-After`; the retry with the same `request_id` replays the worker's result (`claim_request` InFlight path, `transcode/manager/create.rs:5595`) instead of a second start; (d) `admit_live_with` returns as soon as a non-hardware decision is final instead of sleeping the whole `QUEUE_WAIT` (`transcode/manager/start.rs:304-313`). **D2b**: `QualityCatalogRequest::is_valid` 16-row cap (`media_pool.rs:211-277`) → compact the client's video rows server-side before validation (TCL RCA). Tests: header honoured (budget = header); capacity refusal carries the code; timed-out create + retry = one producer (count spawns); admission returns early on `Software` decision; 40-row caps accepted. Acceptance: `plurx_start_outcomes_total{outcome="failed"}` with codeless bodies → 0; create ledger `elapsed_ms` p99 falls. Size M, Opus.

**G1 — Activity → Starts.** Root cause: the per-start phase ledger exists only as the `create startup read budget finished` log line (`media_pool.rs:365-440`, `http/hls/create.rs:2963-2973`) and the start ledger (`playstart.rs`) is metrics-only. Change: persist per start, node-locally through telemetry (bounded, batched — see Sept review C15): playback id, title, client, route taken (vod/rolling/progressive), the refusal that chose it, phase string, owner node, TTFF, outcome; `GET /api/v1/activity/starts` (paged, admin); web Activity tab "Starts" with the title's index state and an "index now" action (`/api/v1/analysis/jobs/{id}/retry`). Apple/Android read-only later. Tests: route handler test; web test for the tab against the shipped scripts (`tests/web`). Acceptance: the next slow start is explained from the UI without logs. Size M, Sonnet (web) + Opus (server).

**R1 — "Authority fence" means "client left".** Investigated 2026-11-07 on media1's logs: every one of the 16 `authority_fence won` retirements reads `requested_reason="released by client" winning_reason=authority_fence participation=Won`. Root cause: client release and admin stop retire a session by `session.project_authority_fence()` → `control.fence_unavailable()` (`transcode/manager/control.rs:1021-1027, 1371, 1462, 1512, 1534, 1828-1883`; `transcode/rolling/session.rs:1783`), so the actor's terminal cause is `AuthorityFence` and the retirement owner labels it so (`transcode/rolling/retirement.rs:844-865`, default at `:858`). The metric built to prove a leader restart never interrupts playback therefore cannot distinguish a viewer leaving from a fence — the opposite of its doc comment. Change: typed `RollingTerminalCause::Released` and `::Stopped` for client release and admin stop; `project_authority_fence` reserved for real serving-authority loss; the `end` event stops counting as `already_terminal` for a release. Tests: a client DELETE settles with cause `released`; a serving-fence loss still settles `authority_fence`; `tests/playback/rolling-producer-owners.toml` recount. Acceptance: `authority_fence won` stays 0 across a day of ordinary viewing. Size S, Opus.

**H1 — Stale status lines.** STATUS.md and the docs claim "not deployed" for #1366/#1374/#1376/#1341/#1299, which the fleet runs. Correct them; add the Starts/TTFF before-after rule to this plan's PR checklist. Size S, Sonnet.

### Phase 2 — the fix (1–2 weeks)

**A1 — One copy engine; the first play builds the index.** Root cause: copy without an index is refused by VOD (`vod/serve/create.rs:~659-830`) and served by the rolling engine, whose first publication waits for `bootstrap_ms` ≈ 16 s of media (`transcode/rolling/publication.rs:164-175`; `ROLLING_PRESENTATION_TARGET_SECS = 16`, `crates/plurx-core/src/transcode/mod.rs:216`). Change: a copy start with no index attaches a VOD rendition whose producer is the sequential remux (`vodgen::run` cutting at keyframes as they arrive instead of at a precomputed plan); the cutter records each fragment (keyframe byte offset, timestamp, sample sizes) into an incremental index published head-first (`cluster_fragment_index_jobs` artifact rows, same validation as the background builder); segments are servable from segment 0; the plan is fixed once the index is complete and the rendition becomes an ordinary indexed one (same `rendition_key`, so a second viewer joins it). Background indexing (`resolve_analysis_requests`) keeps its role for unplayed titles and must dedupe against a play-built index. The rolling copy producer for library titles is retired; Live TV keeps its own. First-publication rule for every engine: 3 complete segments or ENDLIST (promote the `FirstCompleteSegment` control to production as "three segments"; reload coverage comes from `EXT-X-TARGETDURATION` and the growing playlist, which is how every HLS player handles a live/EVENT playlist). Tests: a play-built index byte-equals a background-built one on the fixture corpus (`copyseg` fixtures); first playlist served after the third segment; two concurrent viewers share one producer; the index survives a producer crash mid-title (resume from the last published fragment). Acceptance: `plurx_live_hls_recovery_sessions_total{reason="vod_index_pending"}` stays 0 after deploy; remux TTFF p90 ≤ 5 s cold on media1; `published` index count rises with plays. Size XL, Opus with Fable design review before build.

**A2 — Encoded starts.** Same 3-segment first-publication rule for encoded rolling (hardware encoders exceed real time; software may not — the actor's 12/30 s verdicts stay). Acceptance: transcode TTFF p90 ≤ 5 s on a hardware node. Size S after A1.

**A3 — One live document; presentation API.** Write a new `streaming/PLAYBACK-START.md` (§1 contract, clocks table with one symbol per value, engines, acceptance); mark the 22 startup documents superseded in `docs/streaming/README.md`; `presentation` on create becomes advisory and 410 `live_presentation_removed` goes away (ruling 1: either path is first class because there is one path). Size S, Sonnet.

### Phase 3

**B1 — Index identity without the engine.** Root cause: `fragment_index_engine_digest` hashes the ffmpeg binary, `-version` output and every loaded object (`ffmpeg.rs:3224-3260`); `pipeline_version` and `result_cache_key` carry it; `fragment_index_engine_is_current` retires everything on change. Change: key = (file id, size, mtime, container-parse digest of the keyframe table, `FRAGMENT_INDEX_CONTRACT` — a hand-bumped constant naming the muxer flags that change sample framing); serve-time `establish_or_verify` (`vod/generation.rs:~1267`) compares the first init/segment against the index's recorded sizes and, on mismatch, rebuilds *that title* through A1; existing indexes under engine-keyed identities are adopted after the `index_validation_backfill` check instead of discarded. Processed DV roots stay keyed by recipe — that is correct, their bytes do change. Tests: a fixture index built under one engine digest is adopted under another after validation; a contract bump invalidates; a corrupt sample size triggers a per-title rebuild and nothing else. Acceptance: a deploy with no contract bump leaves `published` unchanged and `retry_wait` flat. Size L, Opus with Fable review.

**D3 — Create collapse.** One `StartPlan` from one `playback_planning_snapshot` (reuse `CreateStartupBudget.binding`, `media_pool.rs:561`, for every later consumer; `vod_preparation_snapshot` and `validate_candidate_planning_binding` take it instead of re-reading); the held-source ffprobe per start (`transcode/manager/plan.rs:484`) becomes re-probe-on-open-failure; the quality catalog is a replicated row per (file identity, caps class, node capability snapshot) refreshed by the owner when any input changes, so create reads one row and fans out only when a peer advertises an encoder this node lacks; link receipts and network prior read from the E1 node-local view. Tests: create ledger asserts `authority_reads ≤ 3`, `mutations ≤ 2`; catalog row invalidation on each input. Acceptance: `create startup read budget finished` p99 `elapsed_ms` < 1,000 on the fleet. Size L, Opus.

### Phase 4

**E1 — Replication classes as designed.** Settings, device profiles, media-session routes and telemetry served from a node-local applied view fed by the raft log (vendored hiqlite fork: commit hook or applied-index watch); fences (serving authority, ownership, activation) stay consistent reads; route cache from 1 s TTL (`media_sessions.rs:107`) to epoch invalidation on release/replacement/fence. Tests: a settings write on node A is visible on node B's view within one applied-log tick; a fence never reads the view. Acceptance: `plurx_http_store_reads_total{class="authority_read"}` for `settings` and `playback` down by 10× at equal session counts. Size L, Opus with Fable review.

**F1 — One clock, one retry owner.** Clients: delete the 1/2/4 ladders and the dead 60 s deadline; one start clock per client (web 20 s preparation + startup merged into one; Apple/Android 60 s → one value the Developer tab shows); `Retry-After` obeyed. Server: `PLAYLIST_WAIT_BUDGET`, `SEGMENT_WAIT`, `block_budget` derived from the client's remaining budget and the actor's constants through single symbols (the `ACTOR_*` mirrors in `transcode.rs:244-304` become re-exports); terminal-code lists unified in `tests/playback/playback-surface-contract.json`. Tests: contract JSON is the only place a code's class is declared; `test_control_wire_conformance` extended to the start codes. Acceptance: no client constant duplicates a server one (a literal census in `tests/validation`). Size M, Opus.

**H2 — Fleet hygiene.** Bring lab4 to the fleet build; add a `/api/v1/server` build-skew warning on the cluster panel; note `PROTOCOL_VERSION` exact-version placement in OPERATIONS.md's rollout section. Size S, Sonnet.

## 4. Sequence and gates

C3 → C1 → C2 → D2 (+D2b) → G1 → R1/H1 in parallel → **A1** (design review first) → A2 → A3 → B1 → D3 → E1 → F1 → H2.
Phase 1 lanes are independent and may run concurrently in separate clones. A1 needs D2 landed (the create answers "not yet" correctly while the producer warms). B1 needs A1 (the per-title rebuild is A1's builder). D3 needs E1 for the last two reads but lands its snapshot dedupe first.
Each lane: proper commits, `Agent-Model:`/`Agent-Session:` trailers, draft PR into `effort/playback-start`, one adversarial review per lane deliverable, fast lane once, merge. Effort → main after one unit run. Infra names scrubbed (`scripts/scrub-infra-names --check`), `make paperwork` green before every push.

## 5. Routing

- **Review (Fable, then Astra):** this plan, before Phase 1 starts; A1 and B1 designs again before their build.
- **Build (Opus):** C1, C3, D1, D2, A1, A2, B1, D3, E1, F1, G1 server half, R1.
- **Build (Sonnet):** C2, G1 web, A3, H1, H2.

## 6. Lane status

| Lane | Owner (model, session) | State | PR | Fleet reading before → after |
|---|---|---|---|---|
| C1 | — | unclaimed | — | claims/2.7 d: 16 (media1) → |
| C2 | — | unclaimed | — | verdict: idle → |
| C3 | — | unclaimed | — | active rows 4,097/4,096 cap, queued under retired digests 3,878 → |
| D1 | — | unclaimed | — | DV TTFF vs non-DV → |
| D2 | — | unclaimed | — | codeless failed starts → |
| G1 | — | unclaimed | — | — |
| R1 | — | unclaimed | — | authority_fence won 16 → explained |
| H1 | — | unclaimed | — | — |
| A1 | — | unclaimed (design review first) | — | vod_index_pending fallbacks 18 → ; remux TTFF p90 → |
| A2 | — | unclaimed | — | transcode TTFF p90 → |
| A3 | — | unclaimed | — | — |
| B1 | — | unclaimed (design review first) | — | published after deploy → unchanged |
| D3 | — | unclaimed | — | create elapsed p99 → |
| E1 | — | unclaimed | — | settings/playback authority reads → |
| F1 | — | unclaimed | — | — |
| H2 | — | unclaimed | — | fleet builds equal → |

## 7. Execution log

| Date | Lane | Model / session | Action |
|---|---|---|---|
| 2026-11-07 | plan | claude-fable-5-1 / 01BCWLA2SJPLAmibeaz9L5xb | Plan written from the review; rulings 1–4 recorded. |
