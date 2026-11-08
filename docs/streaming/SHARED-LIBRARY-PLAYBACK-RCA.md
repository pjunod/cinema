# Shared-library playback — why every shared title times out, and five fleet errors found with it

**Status:** open · **Proposes:** shared playback joins
[A1](PLAYBACK-START-PLAN.md#phase-2--the-fix-12-weeks) plus five
independent repairs · **Written:** 2026-11-08 · **Evidence build:**
`09097650f` on the receiver and `b08b5b8ec` on the source cluster

Companion to [PLAYBACK-START-PLAN.md](PLAYBACK-START-PLAN.md), which covers
how playback starts on your own server. This document covers playback from
*another* server's library: why it never starts, and how it joins the same
start design. Sections 4–9 cover five unrelated errors found in the same
logs, each with its own fix. Owner's ruling (2026-11-08): "it needs to fit
in with the overall design and architecture for sure so make shared libs
work with the a1 design", and the other errors must be fixed too.

Names follow the public-mirror rules: media1 · lab3 · lab4 · lab6 are the
source cluster; the receiver is a separate one-node install on maca, on a
different tailnet. The titles were real library titles. Here they are
**title A** (1.9 GB, H.264 1080p Matroska) and **title B** (12.8 GB, HEVC
2160p Matroska with embedded text subtitles).

## 1. What the owner saw

The second Tailscale account and the device share worked. The shared
libraries appeared, every item page opened, and every Play ended about 20 s
later with a stopped player. Three attempts on 2026-11-08, traced through
both servers' logs and Stores:

| UTC | Where | Event |
|---|---|---|
| 17:22:44 | maca | Play title B; the client asks lab6 (ingress) for its decision: remux, HEVC |
| 17:22:46 | media1 | Source start assigned to media1; native text subtitles requested |
| 17:23:04 | maca | Client preparation deadline (20 s) → `owner_stopped` |
| 17:23:22 | media1 | `Source producer startup failed stage="native_tracks" error_class=Unavailable` (36 s) |
| 17:23:23 | maca | Play title A; decision remux, H.264 |
| 17:23:42 | maca | Client preparation deadline → `owner_stopped` |
| 17:23:52 | media1 | Title A's VOD session attaches, **28 s** after the start, with no viewer left |
| 17:28:45 | maca | Retirement of the abandoned start: `receiver Source status refused … http_status=429` |

All three receiver media sessions ended `revoked`, and none was ever
published. On the source, the only Source session ended `deleted`. Its
`publication_ready_at_ms` was `i64::MAX`, so it never became ready.

## 2. Root cause — a shared start reads the whole file before its first frame

On your own server, a copy start of a title with no index plays straight
away: today through rolling or progressive remux, and after A1 through the
one engine whose first play builds the index. A shared start has neither
path. The Source actor finishes whole-file work *before* VOD admission, and
nothing on the receiver or client side allows for that wait.

```
receiver (maca)                 source ingress (lab6)          owner (media1)
POST …/hls/sessions  ──────────▶ start (305 s deadline) ──────▶ source actor
  client clock: 20 s                                            ├ native_tracks: one full
                                                                │  demux per text track
                                                                ├ cold index: whole-file
                                                                │  remux scan (~70 MB/s)
                                                                └ VOD admission → publish
          ◀── nothing within 20 s; client stops and the receiver revokes ───┘
```

### 2.1 The Source refuses an unindexed copy by building the index first

[`prepare_source_cold_index`](../../crates/plurxd/src/transcode/source_actor.rs)
(`source_actor.rs:1088`) runs at `startup_stage = "recipe_preparation"`.
When `cluster_fragment_index_artifacts` has no row for the file, it calls
`fragindex::start_source_index` and waits for the complete outcome. The
scan reads the whole file. Title A is 1.9 GB and took 28 s. A 2160p remux
of 50 GB would take ten minutes or more. The scan exists only because
`prepare_source_rendition` (`vod/serve/create.rs:2568`, refusal at `:2841`)
keeps the same `vod_index_pending` refusal as the local VOD create
(`:862`). Locally that refusal falls back to rolling. On the Source it
became a blocking pre-step.

On the source cluster, 3,237 of 6,374 files had no fragment index on
2026-11-08. A shared play of any of them cannot start inside a 20 s client
budget.

### 2.2 Native subtitles are extracted whole, one demux per track, before start

When the client asks for native subtitles (any title with an embedded text
track), [`prepare_source_native_tracks`](../../crates/plurxd/src/transcode/source_actor.rs)
(`:1224`) runs [`extract_native`](../../crates/plurxd/src/transcode/source_subtitles.rs)
(`source_subtitles.rs:185`). That runs a full-file FFmpeg demux for
**each** supported track, one after another, before any video work. It
uses neither the Matroska cue-index reader
([SUBTITLE-CUE-INDEX-RCA.md](../clients/SUBTITLE-CUE-INDEX-RCA.md)) nor the
windowed bridge that local playback uses. On title B the extraction failed
after 36 s, with `Unavailable` at the start budget.

### 2.3 Some titles can never be indexed, and the Source has no other path

Title B's index jobs failed on three nodes between 2026-10-07 and
2026-10-22, each with the same diagnostic:
`Non-monotonic DTS; previous: 2000, current: 672` at the first packets,
`covered_ms=166`, FFmpeg exit 234. That class is not rare:

| Fleet Store, 2026-11-08 | Count |
|---|---|
| Files with `index_process_failed` | 299 |
| …of which the stderr tail shows non-monotonic DTS at the head | 262 |
| Files whose only index jobs failed, with no artifact | 987 |

Locally such a title falls back to rolling or progressive remux, which
answers the "any title plays through either path" ruling. Per design this
was not replayed for title B today. The Source has only the indexed-VOD
path, so these titles can never play shared. **This matters to A1 as a
whole, not only to sharing:** once rolling copy and indexed copy are one
engine, the cutter must handle what rolling handles today, or A1 removes
local playback for these 262 titles as well.

### 2.4 The client's clock, the Source's clock and the codes do not meet

- The web start runs inside a 20 s preparation owner
  ([`openSessionRetryingNotYet`](../../crates/plurxd/src/web/player/directed-change.js),
  `directed-change.js:1070`). Shared starts go through
  [`shared-decision.js` `read()`](../../crates/plurxd/src/web/core/shared-decision.js)
  (`:41`, 310 s fetch timeout). It sends no `x-plurx-startup-remaining-ms`,
  the budget header D2 relies on (`http/hls/create.rs`).
- The receiver waits up to 305 s
  ([`shared_receiver_ingress.rs` `start`](../../crates/plurxd/src/http/shared_receiver_ingress.rs)),
  and so does the Source (`shared_source_playback.rs:1714`). Neither knows
  the client's remaining budget. Neither answers with the typed retryable
  "not yet" codes the web ladder understands (`create_503_not_yet` in
  [`playback-policy.js`](../../crates/plurxd/src/web/playback-policy.js)).
- When the client stops, the receiver revokes the start. If the Source
  finished anything, such as title A's index, that work is still subject to
  the abandoned start's cleanup.

## 3. Fix — shared playback becomes an A1 consumer, not a second start path

A1 already changes the shared VOD create code that both paths call: a copy
start with no index attaches a growing rendition whose producer is the
remux, and the first play builds the index. The Source must take that same
path, with no Source-only pre-work. A1 gains these items. The design review
of A1 + B1 covers them, and §3.3 holds their acceptance.

### 3.1 Items added to A1

| # | Change | Where | Why this shape |
|---|---|---|---|
| S1 | Delete the Source cold-index pre-step. `prepare_source_rendition` takes A1's growing-rendition branch exactly as the local create does; remove the duplicated refusal at `vod/serve/create.rs:2841` so there is one copy decision. | `transcode/source_actor.rs:1088-1221` (called at `:1438`); `vod/serve/create.rs:2568` | One engine means one admission rule. A Source that keeps its own pre-step is the second start path the ruling forbids. |
| S2 | The growing rendition's index completes under the Source's existing fenced claim. Partial ranges stay rendition-local (A1 iv). The complete index publishes through `put_fragment_index` as today, and A1 (v)'s HEVC attestation gates only that publication. | `source_actor.rs` (HEVC proof block `:1186-1211` moves to publication) | A cold HEVC shared start then plays inside budget, as A1 (v) already requires locally. |
| S3 | Native text subtitles no longer gate start. The Source publishes the subtitle group from track facts it already holds and serves each WebVTT segment through the windowed reader (cue-index fast path, demux fallback). Whole-track extraction becomes the existing background `subtitle_source` job. `extract_native` and the `native_tracks` stage are removed. | `transcode/source_subtitles.rs`, `source_actor.rs:1415-1425`; Source resources in `plurx-core/src/sharing_resources.rs` | This matches local delivery and the cue-index fix. One demux per track over a 12.8 GB file cannot fit any start budget. |
| S4 | The cutter tolerates a non-monotonic DTS head the same way rolling does today. Before the first cut, record which rolling argv property makes it tolerant (input/output timestamp flags) and add a corpus fixture with a reordered head. A title that still fails gets a defined per-title fallback (ruling 2), served by the same producer through its existing degradation, never a refusal. | `plurx-core/src/transcode/mod.rs` (`copy_index_shared_args_with_input`), `fragindex.rs:1666` | Prevents A1 from turning 262 title failures into playback refusals, shared or local. |
| S5 | The receiver forwards the client's start budget. `shared-decision.js` sends `x-plurx-startup-remaining-ms`. The receiver carries the remaining budget on the wire to the Source, and the Source applies D2's create-budget rule. A Source "not yet" (`vod_index_pending` retires with A1; capacity, owner transition) reaches the client with the same typed code and `Retry-After` as a local create. | `web/core/shared-decision.js:41`, `http/shared_receiver_ingress.rs:208`, `http/shared_source_playback.rs:1707` | One clock per client (F1) has to cross the relay. Otherwise shared starts keep a 305 s server wait under a 20 s client. |
| S6 | The receiver relay accepts a growing playlist. `EVENT` with `ENDLIST` appended later passes through the projection, and the receiver does not cache a playlist as immutable until the Source marks it VOD. | `http/shared_receiver_assets.rs` (`project_manifest`, `serve`), `plurx-core/src/sharing_resources.rs:513` | The rewriter already admits `EVENT`. The receiver's caching rules must follow A1 (i). |
| S7 | Locate asks only the nodes that may own a Source start (§8). | `http/shared_source_forwarding.rs:1948` | A refused RPC per start, on every start, is noise that hides real refusals. |

### 3.2 Non-goals

- **No longer client timeout for shared titles.** A 300 s spinner on a 4K
  title is the same failure with more waiting, and it would mask any later
  regression of the cause.
- **No shared-only rolling path.** A rolling Source producer next to A1 is
  the two-engine design A1 retires.
- **No pre-start subtitle work of any kind.** Not even "only the selected
  track": a single 50 GB demux still misses every budget.
- **No change to pairing, grants, pinned TLS or ingress custody.** They
  worked end to end in this incident.

### 3.3 Tests and acceptance

**Tests (in A1's corpus):**

- A real B-to-Source fixture (`shared_receiver_fixture.rs`) starts an
  unindexed copy and gets its first playlist after the third segment.
- The same fixture with a text track: start does not wait for extraction,
  and the cue segment for t=0 is served.
- A reordered-DTS-head fixture plays through the one engine.
- The budget header crosses the relay: a fake-clock test where the Source
  answers typed "not yet" inside the client's remaining time.
- A receiver that abandons a start leaves a partial range that a later
  viewer reuses or that cleanly settles, never a half-published index.
- **Counterfactual** (house rule): restore `prepare_source_cold_index` as a
  blocking pre-step, and the first-playlist test must fail.

**Acceptance (fleet, after the owner deploys):** on the two-account setup,
title A, title B and one reordered-DTS-head title each reach first frame
within the A1 remux bound (≤ 5 s share of eligible attempts) from cold. A
second shared viewer reuses title A's first-play index. The receiver logs
no `revoked` start for a viewer who is still waiting.

## 4. Predictive preparation fails while any shared session is active

**Symptom:** every minute, lab6 logs `predictive preparation deferred
error=database error: invalid background job row: invalid type: null,
expected i64`. It started at 17:24:34, as soon as the Source session
existed, and stopped when its lease expired. No predictions are made for
anyone during that time.

**Root cause:** the `viewers` CTE in
[`background_jobs_preparation.rs` `demands`](../../crates/plurx-core/src/store/background_jobs_preparation.rs)
(`:55-60`) reads every active `media_sessions` row. A Source session has
`principal_kind = 'sharing'` and `user_id = NULL`: the Store shows one such
row, against 19 `local` integer rows. That row becomes
`{"kind":"viewer","user_id":null}`, and `PreparationDemand::Viewer {
user_id: i64 }` fails to decode. Because `rows.iter().map(decode).collect()`
fails as a whole, the entire tick is lost. The same CTE also reads
`$.request.file_id`, a path the Source recipe does not have.

**Fix:** filter the CTE to `principal_kind = 'local'`. Viewer demand means
"this account's next episode", and a sharing principal is not an account on
this server. Shared viewers' predictions are a feature with their own
design, not a side effect of this query. Audit every other Store query that
reads `media_sessions.user_id` or `recipe_json` paths without a principal
filter (`rg "FROM media_sessions" crates/plurx-core/src/store`), and fix
the same defect wherever it appears.

**Test:** a store contract test (both backends) with one active sharing
session and one local session. It returns exactly the local viewer's demand.
Counterfactual: without the filter, the same test fails with today's error.

## 5. One background source reader on lab3 can never be recovered

**Symptom:** all four nodes log `Shared source reads need recovery; affected
media jobs cannot start`, and `plurx_background_source_recovery_pending` is
1. The sample is cluster-wide, so every node reports the one record.

**Evidence:**

- `background_source_custody` holds custody `16da3798…`, phase `active`.
- It belongs to a `dv_title_scan` job (`a68e6605…`, file 6323, 68 GB) owned
  by lab3's previous process boot. The job is still `running`, with its
  lease expired at 04:35 UTC.
- `source_io_occupancy` still charges the slot, 13 h after expiry.
- lab3's recovery journal holds the activation at
  `source-custody-recovery/v1/background-16da3798….json`, written with
  `"machine_id":"","boot_id":""`.

**Root cause:** the activation was written by a pre-witness build while the
container had no host-identity bind. Recovery replay
([`recovery.rs` `verified`](../../crates/plurxd/src/source_custody/recovery.rs),
`:462`) needs a valid prior machine and boot identity and a *different*
current boot. An empty identity can never satisfy it, and lab3's host has
not rebooted since 2026-10-12, so no v1 reboot proof can exist. There is no
v2 witness either. That is exactly the case
[SOURCE-CUSTODY-RECOVERY.md §1.2](../server/SOURCE-CUSTODY-RECOVERY.md)
covers with the host-admin physical-stop proof, and that proof was never
run on lab3. `/run/plurx-physical-stop` does not exist there, and the
container has no bind for it.

**Fix:** no code change. The documented remedy is the root fix: it proves
the old reader is gone instead of releasing on a guess. In order:

- on lab3, run `scripts/source-custody-reader-stop --check`, then
  `--publish`, as root;
- add the read-only `/run/plurx-physical-stop` bind to lab3's Compose
  override in the deployment playbooks;
- redeploy lab3.

The daemon then settles the custody, clears its journal and frees the slot.
This is the owner's step (deploys). The deployment-playbook change is in
§10.

**Prevention check:** confirm that no current build can write a v1
activation without identity. Grep for writers of the
`source-custody-recovery/v1/` envelope, and add a test that refuses an
empty identity at write time. A future container without the binds must
fail loudly at write time, not strand capacity months later.

## 6. Every node warns about two removed Raft members, every minute

**Symptom:** lab6 logs `hiqlite::split_brain_check: … sqlite node 3 not in
membership config … [1, 5, 6, 7]` for nodes 3 and 4, six lines a minute
(360 an hour).

**Root cause:**

- Each node's `membership.json` keeps the `bootstrap` peer list from the
  moment it joined: lab6 `[1, 3, 4]`, lab4 `[1, 4, 5]`, lab3 `[1, 5, 6]`,
  media1 `[1]`.
- Raft ids 3 and 4 were the earlier lab4 and lab3 identities, removed on
  2026-08-25 and 2026-09-02. Today ids 6 and 7 hold those same addresses.
- [`migration.rs`](../../crates/plurx-core/src/cluster/migration.rs)
  (`:3757-3764`) passes that list to hiqlite as its static `nodes`, and the
  file is never refreshed. The code says so itself at `:1634`: "records the
  bootstrap list once and is never refreshed".
- hiqlite's `split_brain_check` compares the static list with live
  membership every 60 s and warns about each absent id.

The static list is also what a restarting node dials first, and two of its
entries are removed ids that now share an address with live voters.

**Fix:** after the Store opens, the node rewrites its `membership.json`
`bootstrap` from committed membership: `cluster_nodes` with
`removed_at IS NULL`, through the existing `write_local_membership`. Every
node also does this when it applies a join or removal. Startup keeps using
the file as written, so a node that cannot reach the Store still starts as
it does today. Filter removed raft ids from the hiqlite `nodes` at
construction, too, so a stale file cannot reintroduce them.

**Test:** a migration test that removes a peer and restarts. The rewritten
file and the hiqlite `nodes` must both omit the removed id. A second test
starts with a stale file and an unreachable Store, and startup must be
unchanged.

## 7. The leader intermittently cannot confirm leadership for reads

**Symptom:**

- media1, the Raft leader, logs `timeout while confirming leadership for
  read request target=5 … target=6` at the same millisecond for both
  followers, about 28 times in 40 minutes after the 17:00 deploy.
- The followers then log `transient replicated authority read failed;
  retrying retry=QuorumUnavailable`. One of those retries was a user-facing
  `/api/v1/dvr/overview`.

**What is established:**

- Both voters time out in the same instant, and neither logs a stall or a
  slow apply then. A follower-side cause would not hit both at once.
- RTT from media1 to lab6 is 0.64 ms average and 3.8 ms maximum over 300
  pings, with no loss. The two machines share one L2 segment.
- media1's `igc` NIC runs an RX ring of 256, against a maximum of 4,096.
  Its lifetime counters show `missed` = 1,533,976 (FIFO overruns) and
  `dropped` = 510,276. In a quiet 60 s sample `missed` rose by 14 and TCP
  retransmits by 4.

**Leading cause, pending the decisive sample:** under media streaming load,
RX overruns on media1 drop Raft segments. The retransmit (RTO ≥ 200 ms)
then pushes leadership confirmation past its heartbeat window
(`HIQLITE_HEARTBEAT_INTERVAL_MS = 800`). A 0.5 s counter sampler has been
running on media1 since 17:47:48 UTC
(`~/work/rca-nic/nic.log`, 40 minutes, removed afterwards). The result is
recorded in §12.

**Fix if confirmed:** raise media1's RX ring with `ethtool -G enp86s0 rx
4096`, persisted in the host role of the deployment playbooks, and check the
`igc` interrupt coalescing. Acceptance: no read-confirm timeouts over 24 h
of normal playback.

**If not confirmed:** time a read-confirm round trip on the leader. That
means a debug span around openraft's linearizable read, plus a tokio
runtime stall probe. Do not raise the heartbeat or election windows: that
hides the stall instead of removing it.

## 8. Source locate is refused by the learner on every start

**Symptom:** lab3 logs `http request failed status=503 latency_ms=0
path=/internal/cluster/sharing/source/locate` for every shared start.

**Root cause:** lab3 is a learner, raft id 7. `capacity_role_refusal`
([`http/mod.rs`](../../crates/plurxd/src/http/mod.rs), `:2850`) answers
every route outside `learner_route_eligible` (`:2665`) with
`learner_route_ineligible`. `collect_fresh_location` (`:1948`) fans out to
every reachable `media_peers()` member, without checking whether its role
may own a Source start.

**Fix:** use the same role predicate in both places. Locate asks only peers
whose committed role admits Source start ownership. Membership already
carries the role (`cluster_nodes.role`). Today that means voters only. If
learners should serve Source media, the same way they serve node-local media
routes, that is a deliberate extension of `learner_route_eligible`, recorded
in A1's design review. A side effect of fan-out is not the place for it.
Decision for the owner, recommended no for now: learners would also need
Source claim-write forwarding, which this incident does not need.

**Test:** a forwarding unit test with one learner and one voter peer. Only
the voter receives locate. Counterfactual: the current code sends both.

## 9. Shared posters fail in bursts with 503

**Symptom:** maca logged 18 failed `/api/v1/shared/imports/…/art/…` requests.
Each was a distinct image, failing once, in two bursts: six within 12 s at
17:07, and twelve within 29 s at 17:29–17:30. Latency was 290–630 ms, so each
reached the Source and came back refused.

**Root cause:**

- Source art admission is a non-waiting
  `try_acquire_owned` on `SOURCE_OPERATIONS = 4`
  ([`shared_artwork.rs`](../../crates/plurxd/src/http/shared_artwork.rs),
  `:20`, `:74`). The receiver's side is the same, `RECEIVER_OPERATIONS = 4`.
- A library grid asks for dozens of posters at once. The Source answers
  everything past four in flight with `429 sharing_art_capacity` or
  `sharing_art_variant_busy`.
- The receiver then turns every peer error into 503: `read_artwork(…)
  .map_err(|_| unavailable())` (`:240`).
- The page gets a broken image and no retry signal.

**Fix:**

- Make both admissions bounded waits rather than instant refusals: wait for
  a slot within a short deadline (1–2 s, set from the byte budget), keeping
  the same operation and byte limits.
- Carry the Source's typed refusal through the receiver as 429 with
  `Retry-After`, never 503.
- The shared artwork loader retries a 429 once after `Retry-After`. Once one
  fetch succeeds, the receiver's disk snapshot serves the image after that.

**Test:**

- 24 concurrent art requests against one Source fixture all succeed within
  the deadline.
- A forced Source 429 reaches the client as 429.
- Counterfactual: `try_acquire` refuses 20 of the 24.

## 10. Order of work and owners

| Item | Lane | Size | Blocked on |
|---|---|---|---|
| §3 S1–S7 | A1 (design review with B1, Fable; then Opus build) | adds ~M to A1's XL | A1 design review |
| §4 predictive preparation | standalone PR to main | S | — |
| §6 membership bootstrap | standalone PR to main | S–M | — |
| §8 locate role filter | standalone PR to main (S7 reuses it) | S | — |
| §9 shared art admission | standalone PR to main | S | — |
| §5 lab3 custody | owner: host helper + playbook bind + lab3 redeploy | ops | — |
| §7 media1 NIC | owner: playbook host change, after §12 confirms | ops | sampler result |
| §5 prevention test | with §4's PR | S | — |

The four standalone PRs touch disjoint files, so they can stack into one
fast-lane run. The PLAYBACK-START-PLAN A1 row gains "S1–S7, see
SHARED-LIBRARY-PLAYBACK-RCA.md §3" in the same commit as this document.

## 11. Decisions for the owner to look over

1. **Learners stay out of Source start (§8).** Recommended now. Revisit in
   A1's design review if learner media serving widens.
2. **The lab3 custody is released only through physical-stop proof (§5),
   never by a manual Store edit.** That is the custody doc's rule, applied
   here as written.
3. **Shared viewers get no next-episode predictions (§4) until that is a
   designed feature.**

## 12. Evidence ledger

| Date | Evidence | Scope |
|---|---|---|
| 2026-11-08 | Receiver and source logs, both Stores (read-only snapshot copies, since removed), the media1 / lab3 / lab4 / lab6 membership files, lab3's recovery journal, ping and NIC counters. No setting, file or service was changed on any node. | §1–§9 |
| 2026-11-08 | NIC overrun sampler on media1, 17:47:48–18:27:48 UTC | §7, result below |
