# Cinema end-to-end review — why playback starts are slow or fail, and what to change

**Status:** open — assessment for Paul; the design decisions in §6 are his.
**Verified against:** `origin/main @ 8c654d37b5ee` (2026-11-06) and the live fleet on 2026-11-07 (media1/lab6 on `plurx-player-v0.3.2-424-g4ddbf1229`, lab4 on `…-410-gedf6d08fa`, lab3 a learner).
**Reviewer:** Claude (Fable 5.1), session `01BCWLA2SJPLAmibeaz9L5xb`. Code read at HEAD; fleet read from `/metrics` and `/api/v1/server` only (no logs, no DB, nothing deployed or changed).
**Scope:** the playback start path top to bottom — client click → `/decision` → create → placement → VOD or rolling engine → playlist/segments → first frame — and how the last month's HDR/DV, encoding and capability changes sit on it.

---

## 1. Bottom line

The slow and failed starts are not a bug that one more fix will catch. They are the designed behaviour of the current start path:

1. **The fast path (indexed VOD) depends on an artifact the system is built not to produce.** VOD copy needs a fragment index. The indexer is a background queue that yields to every foreground reader, shares two fleet-wide source-I/O slots with HEVC proofs, subtitle extraction and DV scans, stops claiming on any node that has an encoder in use, and keys every index on a SHA-256 of the exact ffmpeg binary plus every loaded `.so`. On the fleet right now the index queue has **13.6k published, 7.7k failed, 3.9k in retry-wait**, reports its health as **"idle"**, and has made **16 claims in 2.7 days** on the serving node.
2. **So most starts take the slow path.** On media1 since the last deploy: **18 sessions fell to the rolling engine for `vod_index_pending`** against 11 VOD copy generations. The rolling engine will not publish its first playlist until it has ~16 s of media (`ROLLING_PRESENTATION_TARGET_SECS = 16`). Measured TTFF on that node: Safari remux **7 of 8 starts took 10–30 s** (mean 12.9 s); Apple remux 2 of 4 over 5 s; Apple transcode 2 of 5 over 10 s. That is the "slow" you are seeing. The "fails outright" is the same wait colliding with a client deadline (web 20 s preparation + 40 s startup, Apple/Android 60 s, server create 10 s) — see §3.
3. **Every startup repair of the last month tuned the slow path** (publication timing, Resume ownership, pending-family retry, read-ahead) or the create path's book-keeping. All of them are deployed (#1366, #1374, #1376, #1341, #1299 are in the running build; the "not deployed" lines in STATUS.md and the docs are stale). They were correct fixes to real defects and could not move the number, because the number is set by which path a start takes.
4. **The create path has grown into a 10-phase, ~13-decision transaction** with 20–40 quorum reads and several writes, a synchronous DV render-and-encode trial inside it, and a 10 s budget nobody on the client side knows about. It works, but it is the opposite of ARCHITECTURE.md §3 ("get out of the way first"), and it is where the HDR/DV and capabilities work landed.
5. **Consensus is still used for everything** (Sept 20 review finding S1, open): on media1 in 2.7 days, 168k quorum reads on playback routes, 82k on settings, 149k on cluster routes, 229k "other", against 0.8k playback writes; 68% of HLS media GETs go to the Store because the route cache TTL is 1 s.

The design adjustment that fixes this is not another gate. It is: **make the first play produce what the fast path needs, instead of waiting for a background queue to have produced it; make the index survive a deploy; and shrink the create to one read, one decision, one write.** Details in §6.

---

## 2. What the fleet says (2026-11-07, read-only)

Counters are process-local to media1 (uptime 2.7 d) unless marked fleet-wide (replicated table).

| Signal | Value | Meaning |
|---|---|---|
| `plurx_ttff_ms` remux/safari | 8 starts; 1 ≤5 s, 7 in 10–30 s; sum 103 s | mean 12.9 s |
| `plurx_ttff_ms` remux/apple | 4 starts; 2 ≤5 s, 2 in 5–10 s | mean 6.0 s |
| `plurx_ttff_ms` transcode/apple | 5 starts; 3 ≤2.5 s, 2 in 10–30 s | bimodal: cached vs not |
| `plurx_start_outcomes_total` | remux ok 7 / failed 1; transcode ok 3 | |
| `plurx_live_hls_recovery_sessions_total{reason="vod_index_pending"}` | **18** | starts that fell to rolling because no index |
| `plurx_vod_producer_generations_total{kind="copy"}` | 11 | VOD copy starts |
| fragment-index requests (fleet-wide) | published 13,643 · failed 7,673 · retry_wait 3,879 · canceled 3,515 | ~27% of the library terminally failed, ~14% waiting |
| `plurx_analysis_queue_health` | `idle` on all three voters | verdict reads `cluster_fragment_index_jobs`; the depth gauge reads `analysis_requests` — different tables, so "idle" with 3.9k waiting |
| `plurx_analysis_queue_claims_since_start` | 16 (media1, 2.7 d) · 16 (lab4, 2.7 d) · 13 (lab6, 1.7 h) | the idle node claims 100× faster than the serving node |
| `plurx_http_store_reads_total{authority_read}` | playback 168k · settings 82k · cluster 149k · other 229k · item 128k | ~0.7 quorum reads/s on playback alone, continuously |
| `plurx_media_session_route_lookups_total` | store 15,144 · cache_hit 7,205 | 68% of HLS GET route lookups are quorum reads (1 s TTL) |
| `plurx_playback_rolling_terminal_events_total` | `authority_fence won` 16 · `end already_terminal` 18 · `pause_expired won` 2 | 16 of 18 rolling sessions were ended by a serving-authority fence, not by their viewer — needs log confirmation; #1341 is in this build |
| HTTP playback routes | GET 2xx 20,049 · 4xx 2,323 · POST 2xx 9,660 | handler p99 < 250 ms, max bucket 10 s — the wait is in media publication, not in handlers |
| Fleet versions | lab4 `edf6d08fa` vs media1/lab6 `4ddbf1229` | mixed-version fleet; `media_pool::PROTOCOL_VERSION = 9` places exact-version only |

The HTTP handlers are fast. The time goes into (a) which engine the start lands on and (b) that engine's first-publication gate, with the client polling or giving up around it.

---

## 3. The start path as it is at HEAD

### 3.1 Server create (`http/hls/create.rs::create_with_purpose_inner`, 1,800 lines, lines 3039–4859)

Phases, each a `mark_create_phase`: **lookup → settings → plan → admission → predecessor → processed_qualification → placement → dispatch → activation → registration**, plus a *worker → vod → rolling* sub-ledger inside dispatch. The per-create phase ledger is logged as `create startup read budget finished` (`elapsed_ms`, `authority_reads`, `local_reads`, `mutations`, `phases`) — this line is the single most useful thing in the logs and is surfaced nowhere in the product.

What runs before the response:

| Step | Store / I/O | Note |
|---|---|---|
| `record_desired_selection` | quorum write | before anything is validated |
| `playback_planning_snapshot` (file + probe + 25 settings) | quorum read | read again inside `validate_candidate_planning_binding`, again in dispatch, again in `vod_preparation_snapshot` — 3–4× per start |
| `plan_review_for`, `capability_height_ceiling…` | — | caps v2 reconciliation |
| `stored_prior` (network prior) | quorum read | |
| `resolve_plan…` → `quality_catalog` | fan-out to every peer, `QUALITY_CATALOG_DEADLINE` 2 s, 1.5 s per peer | TCL RCA measured ≥80 serial authority reads here; 16-row cap silently rejects Android's uncompacted caps |
| `link_receipts::request_binding_until` | quorum read | advisory |
| `claim_media_session_request` | quorum write | first durable admission |
| predecessor route read, `admit_restart` | quorum read | |
| **`prepare_processed_hdr10`** | planning snapshot read + **spawn renderer helper + encoder, produce and verify the first processed window** | `create_enhancement_deadline` ≤ 8 s, half the remaining budget; on failure degrades to ordinary after spending it |
| quality catalog (second pass) | fan-out | if still `None` |
| placement: `media_pool.offers` | peer fan-out, 200 ms | |
| `create_cluster_session` → `create_session_inner` | `vod_settings` read, `try_vod_session` (planning snapshot again, held-source **ffprobe at every start**, 5 s; engine/executable attestation; `prepare_recipe`; cluster index lookup; `attach_rendition_owned` under a per-key gate) → on refusal **`start_live_recovery_session`** (re-validate binding, `get_file`, `suspend_playback_analysis_interest` write, `admit_live_with` which **always sleeps the full 5 s `QUEUE_WAIT`** on any non-hardware outcome, encoder choice, plan resolved twice, spawn) | this is where a VOD-refused start pays for both engines |
| `assign_media_session_request_owner`, `publish_media_session_activation` | quorum writes | |

Budget: `CreateStartupBudget::new(min(x-plurx-startup-remaining-ms, 10_000))` — **no client sends that header**, so every create has a 10 s budget; `START_DEADLINE` (50 s) is clamped to it (`create_stage_deadline`). On expiry the client gets 503 `startup_timeout` and the detached worker keeps going; a late success is dropped with its guard, so the encoder slot was spent for nothing. A capacity refusal is a *codeless* 503, which no client retries, while the timeouts are coded and retried — inverted.

### 3.2 The two engines

- **VOD** (`vod/*`, `vodserve.rs`): one long-lived ffmpeg per rendition cutting at a fixed plan; segments served from disk, blocked GETs wait `block_budget` 8 s then 503 `segment_pending` (hls.js TTFB is 10 s). Copy needs the fragment index and `duration_ms > 0`; HEVC copy needs a proof. No index → `vod_index_pending` / `vod_index_unavailable` → rolling (when `VOD_LIVE_RECOVERY`, default on).
- **Rolling** ("temporary live-HLS recovery"; the public live presentation now returns 410 `live_presentation_removed`): prepublication actor with a 12 s hardware / 30 s software start verdict, `PROGRESS_STALL` 10 s, first publication at `bootstrap_ms` ≈ 16 s of media (`FirstReloadWindow`; `Conservative` 48 s is now test-only). The playlist handler holds the request up to `PLAYLIST_WAIT_BUDGET` 55 s (= 12 + 30 + 13 mirrors of the actor constants), then 503 `startup_timeout`. Segment GETs wait 20 s inside a 35 s request lifecycle.

So a copy-of-a-remuxable-file that has no index takes: VOD attempt (snapshot, ffprobe, attestation, index miss, enqueue) → fallback → 5 s admission sleep → spawn → 16 s of media before the first playlist. At 1× remux speed that is the 13 s mean we see; under NAS contention (the AC3 retest RCA measured 0.9× ingest) it crosses 20 s and the web client's preparation clock.

### 3.3 Clients

Web: `/decision` + create under one 20 s preparation clock; create retried on six "not yet" codes with a 1/2/4 s ladder and a 60 s deadline that is unreachable because the 20 s clock wins; `HLS_STARTUP.cold_deadline_ms` 40 s after attach; `vod_index_pending` on an MSE browser jumps straight to progressive remux (a third engine) with its own 20 s. Apple and Android: same six codes, 1/2/4 s, a real 60 s deadline; Android startup deadline 30 s. Nobody sends the remaining-budget header. Terminal-code lists differ between create and playlist (`media_owner_transition` retried on one, terminal on the other).

Eight independent clocks on one start: server create 10 s · admission 5 s · actor 12/30 s · playlist 55 s · segment 20/35 s · rolling bootstrap 16 s of media · client 20 s + 40 s (web) / 60 s (native) · create ladder 1/2/4. None derives from another.

---

## 4. The last month's changes, and how they sit

About 240 PRs (#1150–#1393, ~3,300 commits) merged since 2026-10-07 from Codex, Claude and GPT sessions.

| Area | What landed | Fits the design? |
|---|---|---|
| **Display-targeted DV / HDR10-E / processed HDR10, creative trims, P8.1 FEL reconstruction, CM29 gamut** (#1216–#1289, #1301, #1374) | A renderer helper + encoder per window, a `DvTitleScan` job, display-target policy, processed candidates, 4 instance slots, 1 reconstruction slot | The renderer, title scan and processed-window producer are well isolated. **The hook into create is not**: `prepare_processed_hdr10` renders and encodes the first window synchronously inside the click path (≤8 s), reads the planning snapshot a third time, pins the owner to the ingress node, and holds the source fence for the whole create. This belongs *after* the first frame as an upgrade (the M6 prepared-replacement machinery exists for exactly that), not before it. |
| **Caps v2 / display-aware Auto / candidate catalog / continuous quality** | Negotiated grade from client caps + worker catalog | Right idea (ARCHITECTURE §3: a pure decision function). Wrong place: a cluster fan-out with ~80 authority reads and a 2 s deadline on every click, re-run a second time if empty, with a 16-row cap that rejects real Android caps (TCL RCA). The catalog is a function of (file, caps-class, node capabilities) and changes only when those change — it should be computed once and replicated, not negotiated per start. |
| **Encoding changes** (encoder preference, hardware/software demotion, recipe tokens like `--plurx-dvh1-completeness=1`) | Recipes and identities | Fine as recipes. Each new recipe token and each ffmpeg/dependency change retires every cached index and processed root (the code says so: "restart establishes a new digest and keyspace"). With daily deploys that is a library-wide slow-start event per deploy. |
| **Startup repairs** (#1299 VOD index recovery, #1341 quorum gaps, #1366 cold startup/Resume, #1376 publication timing, #1374 read-ahead) | Deployed | Correct fixes inside the slow path. They cannot change the share of starts that take it. |
| **Live presentation removed from the public API** (410) | Rolling is now only a server-side fallback | Contradicts your 2026-10-04 ruling that any title must play through either path; in practice rolling still carries the majority of real starts while being treated as degraded. |
| **Library worker driver, async Store refinement (parked), admission refinement** (#1358, #1260, #1251) | Shared driver for library workers | Good direction; does not reach the fragment-index claim path, which still gates on `pretranscode_worker_idle` and the two global I/O slots. |

Process signal: `docs/streaming/` holds **22 startup-related RCAs/implementations from different sessions**, most "open", with four unreconciled values for the same first-publication gate (48 s kept / 16 s single bootstrap / 8 s encoded + 24–32 s snapshot / "16 s cannot be lowered"), four different stated causes for "Resume does not start", four retry layers with different owners, and status lines that say "not deployed" for code the fleet is running. Nobody owns the start path; everybody fixes a symptom on it.

---

## 5. Root causes, ranked

1. **Fast path gated on a starved background artifact.** `claim_analysis_request_for_capacity` only claims when the node has no encoder in use (`pretranscode_worker_idle`), only for requests whose `pipeline_version` equals this node's engine digest, only when neither of the two fleet-wide `source_io` slots is held and no foreground permission exists on the resource. The verdict that would tell an operator this reads a different table than the depth gauge. Result: `vod_index_pending` is the common case, and it is the one refusal that costs the most (both engines + 16 s).
2. **Index identity = ffmpeg bytes + every `.so`.** An index records remuxed sample sizes, so it is keyed on the engine. Any deploy that changes a dependency retires the library's indexes; a mixed-version fleet cannot even claim each other's requests. Cache invalidation is the trigger for the slow-start waves after deploys.
3. **The rolling engine's first publication is a policy constant (16 s of media), not a client requirement.** `transcode_first_playlist_ready` needs 2 segments ≥ 4 s; hls.js/AVPlayer play a 3-segment live playlist. The 16 s came from a reload-coverage heuristic and has been argued back and forth in four documents. For copy, the producer is faster than real time, so the gate is pure wait.
4. **Create does advisory and speculative work synchronously** (DV first-window trial, held-source re-probe every start, catalog fan-out, link receipts, network prior, 3–4 snapshot reads) under a 10 s budget the clients don't know about, with the real worker detached past the deadline.
5. **Quorum reads for node-local questions** — settings, routes, catalogue, telemetry — at ~0.7/s on playback alone; 1 s route-cache TTL. ARCHITECTURE §2.2 defines the replication classes; the implementation uses the durable class for everything. (Sept 20 review S1/C9/C15/L2, unchanged.)
6. **Eight uncoordinated clocks and three retry owners** per start, with the retryable/terminal classification inverted for capacity vs timeout.
7. **No product-visible attribution of a start.** The phase ledger exists only as a log line; the Activity page cannot answer "why did this title take 14 s" or "why is a third of my library un-indexed".

---

## 6. What to change — design, not band-aids

Ordered by effect on the number you see. Each names the root cause it removes and how we know it worked.

### A. The first play produces the index (removes 1, most of 3)
Make the index a by-product of serving, not a prerequisite. A copy start without an index runs the *same* sequential remux the rolling engine runs today, but as a VOD-shaped session whose segmenter records keyframe positions and sample sizes as it goes and publishes the index incrementally — head first, so the first segments are servable as soon as they exist (3 segments / ≈6 s of media, not 16 s), and the whole-title index is complete when the pass is. The background queue then only has to index titles nobody has played. This collapses "rolling copy" and "VOD copy without index" into one engine with one publication rule, which is also what retires most of the 22 documents. For *encoded* starts (no index needed) the same first-publication rule applies.
*Acceptance:* `live_hls_recovery_sessions_total{vod_index_pending}` → 0 on the fleet; `plurx_ttff_ms` remux p90 ≤ 5 s cold on media1; the published index for a first-played title verifies against a background-built one.

### B. Index identity independent of the engine binary (removes 2)
Key the index on the source (file identity + container parse) and the *output contract* (muxer settings that change sample bytes), not on a hash of ffmpeg and its libraries. Verify at serve time — `establish_or_verify` already compares the first init/segment — and fall back per title, not per library. Until then: on engine change, run the cheap re-validation (`plurx_index_validation_backfill_total` exists) instead of discarding.
*Acceptance:* a deploy with an unchanged recipe leaves `published` unchanged and `retry_wait` flat.

### C. Give the indexer a floor and an honest verdict (removes the rest of 1)
Let learners and idle nodes claim untargeted index work (ARCHITECTURE §2.4 already says ready learners may index; the claim SQL gives a learner only requests targeted at it, and nothing targets them); drop `pretranscode_worker_idle` as a hard gate on the serving node in favour of the existing pacing/yield; make the health verdict read the same rows as the depth gauge and show the dominant `last_error_code` of the 7.7k failures in Settings → Activity. This is also the one item worth doing *before* A, because it starts repairing the library tonight.
*Acceptance:* `claims_since_start` climbs on every voter; `failed`/`retry_wait` fall daily; the verdict is never "idle" while rows are claimable or waiting.

### D. Shrink create to one read, one decision, one write (removes 4)
One `StartPlan` from one snapshot read (file, probe, settings, caps) through a pure function — the decision engine ARCHITECTURE §3 describes — to one durable claim+activation write, then spawn, then respond. Move off the click path: the processed-DV first-window trial (becomes an in-play upgrade through prepared replacement), the per-start held-source ffprobe (trust the stored probe; re-probe on open failure), the catalog fan-out (precomputed per file × caps-class, replicated; fan out only when a peer has an encoder this node lacks), link receipts and network prior (read from a node-local view). Clients send `x-plurx-startup-remaining-ms`; the server's create returns within it or answers "not yet, retry after N" with its own estimate.
*Acceptance:* `create startup read budget finished` shows ≤ 3 authority reads, ≤ 2 writes, `elapsed_ms` p99 < 1,000 on the fleet.

### E. Replication classes as designed (removes 5)
Settings, device profiles, media-session routes and telemetry read from a node-local applied view fed by the raft log (the vendored hiqlite fork is already patched; a commit hook is in reach), with fences — serving authority, ownership — kept as consistent reads. Route cache from TTL to lease/epoch invalidation.
*Acceptance:* `plurx_http_store_reads_total{class="authority_read"}` for `settings` and `playback` drops by an order of magnitude at the same session count.

### F. One clock, one retry owner per layer (removes 6)
The client owns the TTFF budget and sends it; the server owns "ready / not yet (retry after N)" and never answers a capacity refusal without a code. Derive the playlist and segment waits from the actor's constants (they are already mirrors — make them the same symbol) and from the client's remaining budget. Delete the ladders. Reconcile the terminal-code lists.
*Acceptance:* `tests/playback/playback-surface-contract.json` carries one retry contract; no client constant duplicates a server one.

### G. Attribution in the product (your 2026-07-29 rule; removes 7)
A "Starts" view in Activity: per start — title, client, route taken (vod/rolling/progressive), the refusal that chose it, the phase ledger, owner node, and the title's index state with a one-click "index now"; plus the index queue by state with the verdict and top error codes. It is the same data as the log line.

### H. Process
One owner for the start path and one live document (a new `streaming/PLAYBACK-START.md`: contract, clocks table, engines, acceptance) that supersedes the 22; stale "not deployed" status lines corrected (the fleet runs #1366/#1374/#1376); the mixed-version fleet (lab4) brought level; no further start-path PR without a before/after `plurx_ttff_ms` reading on the fleet.

**Sequence:** C (days, repairs the library in place) → D's DV-trial removal and remaining-budget header (days) → A (the engine unification, 1–2 weeks, the real fix) → B → E → F, with G built alongside A so the next regression is attributable from inside the product. Each is its own effort branch with one unit run; none needs a feature gate.

---

## 7. Decisions that are yours

1. **A** changes the architecture statement: rolling-live and indexed-VOD copy become one engine; the public "live presentation" question (410 today) is settled by that. Yes/no.
2. **B** accepts that an index keyed on the source + output contract may need a per-title fallback when an engine *does* change byte output. Yes/no.
3. **D** moves processed-DV/HDR10-E from "before first frame" to "upgrade after first frame" (a short ordinary-grade lead-in on a DV title before the processed stream takes over). Yes/no — this is a visible product behaviour.
4. **C** lets learners (lab3) and idle voters index. lab3 is your busy box; lab6/lab4 are not. Which nodes.

Everything else in §6 is an obvious call and I would proceed.

---

## Appendix — anchors

- Create: `crates/plurxd/src/http/hls/create.rs:2922-2981` (budget), `3039-4859` (inner), `3713-3866` (processed qualification), `4019-4256` (placement), `5914-6060` (error mapping). Budget: `crates/plurxd/src/media_pool.rs:348-528`.
- Dispatch: `crates/plurxd/src/transcode/manager/create.rs:770-1050` (`create_session_inner`), `:1087` (`start_live_recovery_session`), `:1929-2303` (`prepare_processed_hdr10`), `:4012` (`try_vod_session`); refusal classes `crates/plurxd/src/transcode/live_recovery.rs:68-195`.
- Rolling: `crates/plurxd/src/transcode/manager/start.rs:265-486` (admission), `:628-1769` (start); `crates/plurxd/src/transcode/rolling/publication.rs:102-182` (`ROLLING_PUBLICATION_TARGET`, `RollingStartupPolicy`); `crates/plurx-core/src/transcode/mod.rs:216` (`ROLLING_PRESENTATION_TARGET_SECS = 16`); actor budgets `crates/plurxd/src/playback_control.rs:91-104`; playlist wait `crates/plurxd/src/transcode.rs:242-304`; segment wait `transcode/manager/publication.rs:1742-2230`.
- Index queue: claim SQL `crates/plurx-core/src/store/sqlite/fragment_index_cluster.rs:821-944` (`pipeline_version = ?5`, capacity and viewer clauses `crates/plurx-core/src/store/fragment_index_cluster.rs:15-23, 96-120`); worker `crates/plurxd/src/state.rs:9294-9450` (`MAX_REQUESTS_PER_PASS = 2`, `!pretranscode_worker_idle()`); idle predicate `transcode/manager/construct.rs:1653`; engine digest `crates/plurxd/src/ffmpeg.rs:3007-3040, 3224-3260`; verdict `crates/plurx-core/src/store/mod.rs:1374-1496`; depth vs verdict tables `crates/plurx-core/src/store/hiqlite.rs:5985-6052`; retry defaults `crates/plurx-core/src/store/fragment_index_cluster.rs:954-961`.
- Web client: `crates/plurxd/src/web/player/decode-tiers.js:792-1270`, `player/directed-change.js:890-1260`, `playback-policy.js:960-1141` (`CREATE_RETRY`, `HLS_STARTUP`), `player/measurements.js:71-110` (20 s preparation). Apple `clients/apple/Sources/PlayerControllerSupport.swift:2659-2677`; Android `clients/android/app/src/main/java/tv/plurx/app/player/PlaybackPolicy.kt:200-220`, `PlaybackTelemetry.kt:477-479`.
- Prior review with the same structural findings: `docs/reviews/ARCHITECTURE-REVIEW-2026-09-20.md` §3.2 (S1, S7, S10), C9, C14, C15, L2, §4.1.
- Fleet readings: `/metrics` on 10.42.5.236, 10.42.4.8, 10.42.4.14 at 2026-11-07 ~15:00 ET; saved under `the agent drive (`agent/claude/tmp/metrics_*.txt`)`.
