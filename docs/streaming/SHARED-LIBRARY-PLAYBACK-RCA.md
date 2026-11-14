# Shared-library playback — why every shared title times out, and five fleet errors found with it

**Status:** open · **Proposes:** shared playback joins
[A1](PLAYBACK-START-PLAN.md#phase-2--the-fix-12-weeks) plus five
independent repairs · **Written:** 2026-11-08 · **Evidence build:**
`09097650f` on the receiver and `b08b5b8ec` on the source cluster ·
**Review:** adversarial (Opus 5.5), 24 findings, all applied (§13)

**Continuation (2026-11-09):** Playback-start's S4 (index DTS-head
correction) and S12 (route-eligible locate peers) landed in PR #1475.
PR #1472 carries the independent repairs and relay budget, retry and
abandonment work; its final review is complete. Validation and landing
evidence are tracked in the delivery ledger and PR. S1–S3, S8 and S9 remain A1 work; S5's Source-owned caption windows and the fixture Start503 landed in
[#1490](SHARED-SUBTITLE-CUSTODY-AND-START-503.md). This does not qualify
physical devices. See [the shared-library delivery ledger](SHARED-LIBRARY-PLAYBACK-STATUS.md)
and [the playback-start execution log](PLAYBACK-START-PLAN.md#8-execution-log).

Companion to [PLAYBACK-START-PLAN.md](PLAYBACK-START-PLAN.md), which covers
how playback starts on your own server. This document covers playback from
*another* server's library: why it never starts, and how it joins the same
start design. Sections 4–9 cover five unrelated errors found in the same
logs, each with its own fix. Owner's ruling (2026-11-08): shared libraries
must "fit in with the overall design and architecture … work with the a1
design", and the other errors must be fixed too.

Names follow the public-mirror rules: media1 · lab3 · lab4 · lab6 are the
source cluster; the receiver is a one-node install on maca, on a different
tailnet. Here the titles are **title A** (1.9 GB, H.264 1080p Matroska) and
**title B** (12.8 GB, HEVC 2160p Matroska with embedded text subtitles).
Paths are relative to `crates/plurxd/src` unless marked otherwise.

## 1. What the owner saw

The second Tailscale account and the device share worked. The shared
libraries appeared, every item page opened, and every Play ended about 20 s
later with a stopped player. Three attempts on 2026-11-08, traced through
both servers' logs and Stores:

| UTC | Where | Event |
|---|---|---|
| 17:22:44 | maca | Play title B; lab6 (ingress) decides remux, HEVC |
| 17:22:46 | media1 | Source start assigned to media1; native text subtitles requested |
| 17:23:04 | maca | Client preparation deadline (20 s) → `owner_stopped` |
| 17:23:22 | media1 | `Source producer startup failed stage="native_tracks" error_class=Unavailable` (36 s) |
| 17:23:23 | maca | Play title A; decision remux, H.264 |
| 17:23:42 | maca | Client preparation deadline → `owner_stopped` |
| 17:23:52 | media1 | Title A's VOD session attaches 28 s after the start; a producer spawns |
| 17:28:45 | maca | Retirement: `receiver Source status refused … http_status=429` |

All three receiver media sessions ended `revoked`, and none was published.
The one Source session ended `deleted` with `publication_ready_at_ms =
i64::MAX` (`MEDIA_SESSION_PUBLICATION_BLOCKED`). Title A logged no failure
line. It attached with about 7 s left of its 35 s budget (§2.4) and did not
reach publication. That a Source start can end with no logged stage is a gap
in itself, which S10 closes.

## 2. Root cause — a shared start reads the whole file before its first frame

On your own server, a copy start of a title with no index plays straight
away: today through rolling or progressive remux, and after A1 through the
one engine whose first play builds the index. A shared start has neither.
Its Source actor does whole-file work *before* VOD admission, inside a
hidden 35 s budget, and nothing on the receiver or client side allows for
it.

```
receiver (maca)               ingress (lab6)          owner (media1)
POST …/hls/sessions ────────▶ start ────────────────▶ source actor, 35 s budget
  client clock: 20 s           HTTP waiter: 305 s     ├ native_tracks: one full
                                                      │  demux per text track
                                                      ├ cold index: whole-file
                                                      │  scan, node-local result
                                                      └ VOD admission → publish
    ◀── nothing within 20 s; nothing retryable; the attempt is revoked ──┘
```

### 2.1 The Source has its own copy of create, and its own index

`prepare_source_rendition` (`vod/serve/create.rs:2568-2927`) is a full
second copy of the local `prepare_recipe` (`:620-920`), not a caller of
it. It makes its own choices on probe source, DoVi capability, the cluster
index (skipped for Source, `:2668-2669`), the HEVC gate (`:2715-2786`) and
the key namespace (`source_rendition_key`, `vod/plan.rs:55-77`). It also
keeps the `vod_index_pending` refusal (`:2841`). Locally that refusal falls
back to rolling. The Source instead satisfies it ahead of time:
[`prepare_source_cold_index`](../../crates/plurxd/src/transcode/source_actor.rs)
(`source_actor.rs:1088`, called at `:1438`) checks the **owner node's own
index** (`store.fragment_index`, which on hiqlite is the node-local sidecar,
`plurx-core/src/store/hiqlite.rs:5681-5700`). When it finds nothing, it
scans the whole file and waits for the result.

The consequences:

- **Any title not indexed on that particular owner node cold-scans.** That
  includes titles that have a cluster-wide artifact: 3,224 files have one,
  and the Source never reads it.
- **A scan that does not fit 35 s never completes.** Title A (1.9 GB) took
  28 s. A 50 GB remux is cut off at 35 s, nothing persists, and every retry
  repeats a full-speed read.
- **DV-tagged and non-H.264/HEVC titles skip the scan** (`:1097-1102`) and
  always hit `vod_index_pending`.

### 2.2 Native subtitles are extracted whole, one demux per track, before start

When the client asks for native subtitles,
`prepare_source_native_tracks` (`source_actor.rs:1224`) runs
[`extract_native`](../../crates/plurxd/src/transcode/source_subtitles.rs)
(`source_subtitles.rs:185`). That is one sequential full-file
`-map 0:s:N` demux per supported track (`ffmpeg.rs:1241-1268`), before any
video work. It uses neither the Matroska cue-index reader
([SUBTITLE-CUE-INDEX-RCA.md](../clients/SUBTITLE-CUE-INDEX-RCA.md)) nor the
windowed shared caption route that already exists
(`captions/{asset}/segments`, `http/shared_receiver_assets.rs:56-58`). On
title B it ran out of the 35 s budget.

### 2.3 Unindexable titles: the HEVC index pass aborts on a correctable DTS head

Title B's index jobs failed on three nodes between 2026-10-07 and
2026-10-22, all the same way: `Non-monotonic DTS; previous: 2000, current:
672`, `covered_ms=166`, FFmpeg exit 234. The cause was measured on lab6
with the same FFmpeg and the index's copy options, over 20 s of title B:

| Run | Result |
|---|---|
| index options **with** `-xerror` | `Non-monotonous DTS … aborting`, exit 1 |
| index options **without** `-xerror` | exit 0 (the muxer corrects the head) |

The HEVC index argv adds `-xerror`
(`plurx-core/src/transcode/mod.rs:2692-2694`). Rolling copy does not: it
adds `-xerror` only for strict Dolby Vision (`:1711-1713`). A reordered DTS
head that the muxer corrects therefore makes the index fatal, while rolling
plays the same title. That class is not rare:

| Fleet Store, 2026-11-08 | Count |
|---|---|
| Files with `index_process_failed` | 299 |
| …of which the stderr tail shows non-monotonic DTS at the head | 262 |
| Files whose only index jobs failed, with no artifact | 987 |

**This matters to A1 as a whole:** once rolling copy and indexed copy are
one engine, keeping `-xerror` on the cutter takes playback away from these
262 titles locally as well as shared.

### 2.4 Three clocks that do not meet, and no retry across the relay

- **Client:** 20 s preparation owner
  (`web/player/measurements.js:96`), with the not-yet ladder
  `openSessionRetryingNotYet` (`web/player/directed-change.js:1070`) inside
  it. Shared starts go through `SHARED_DECISION.read`
  (`web/core/shared-decision.js:41`). That builds its own fetch headers
  (`:44`) and so bypasses `web/core/api.js:59`, which sends
  `x-plurx-startup-remaining-ms`.
- **Source:** `source_start_budget_for_request` = `QUEUE_WAIT` 5 s +
  `materialize_budget` 30 s (`source_actor.rs:113-124`,
  `admission.rs:47`, `vod/serve/construct.rs:248`), applied with
  `deadline.min(…)` (`:1387-1392`). The 305 s values
  (`http/shared_receiver_ingress.rs:277`,
  `http/shared_source_playback.rs:1714`) bound only HTTP waiters.
- **No retry:** the receiver maps every Source `PeerError` except DV to
  `Unresolved` (`http/shared_receiver_playback.rs:1554-1557`). Any start
  error retires the attempt as `Revoked` (`:538-549`). The client's retry
  reuses its `request_id` (`directed-change.js:1081`) and so hits the
  settled tombstone (`:621-631`). `SHARED_DECISION.refusal()`
  (`shared-decision.js:14-29`) throws with no `streamFailure`, so the ladder
  never retries a shared create in the first place.
- **Abandonment:** dropping the HTTP waiter cancels nothing
  (`shared_receiver_playback.rs:528-529`,
  `shared_source_playback.rs:1728-1731`). An abandoned Source start holds
  one of 8 registry slots (`shared_source_playback.rs:339`) and its source
  copy permit until its budget ends.

## 3. Fix — shared playback becomes an A1 consumer, not a second start path

The Source must go through the same recipe preparation, engine, index and
clock as a local start. It keeps only what is genuinely Source-specific:
grant authority, custody, the source namespace and the relay. A1 gains
these items. The A1 + B1 design review covers them, and §3.3 holds their
tests.

### 3.1 Items added to A1

| # | Change | Anchors |
|---|---|---|
| S1 | **One recipe preparation.** Fold `prepare_source_rendition` into `prepare_recipe`, with a Source binding as a parameter that carries the namespace, the attesting node and the probe source. Delete `prepare_source_cold_index` and its call. A1's growing-rendition branch then serves both, including non-preserved DV copy. A start with no stored probe evidence gets a bounded header probe in the same step local create uses. | `vod/serve/create.rs:620`, `:2568`; `source_actor.rs:1088-1221`, `:1438`, `:1115-1125` |
| S2 | **One index.** The Source reads the cluster v2 index exactly as local create does. The growing rendition's complete index publishes through the cluster artifact path in B1's format, under the cluster `fragment_index` claim and its fence, so A1 (ii)'s background builder has one claim to join or yield to. HEVC attestation moves to that publication, both the actor block and the admission gate (A1 v). | `source_actor.rs:1135-1216`; `create.rs:2668`, `:2715-2786` |
| S3 | **Producers share by file, not by namespace.** `source_rendition_key` stays for custody isolation. Partial ranges stay per namespace, and there is one index claim per file identity, so a concurrent local and shared cold play build one index, not two racing ones. | `vod/plan.rs:55-77` |
| S4 | **The cutter does not abort on a correctable DTS head.** Drop `-xerror` from the copy cutter's argv. The HEVC header proof's strictness moves to what it must actually refuse: a trace-parse failure or a parameter-set change, already detected by `segplan`/`fragindex`. The rows recorded are the muxer-corrected ones, which are the bytes served. | `plurx-core/src/transcode/mod.rs:2692-2694` |
| S5 | **Subtitles never gate start.** Delete `extract_native` and the `native_tracks` stage. The Source publishes the subtitle group from held track facts and serves cues through the existing windowed caption route (cue-index fast path, demux fallback). It keeps `validate_text`'s bounds per window, and carries a versioned revision identity in place of `source-{inc}-{gen}-{idx}.vtt`. | `source_subtitles.rs:17-20`, `:42-50`, `:185`; `source_actor.rs:1415-1425`; `http/shared_captions.rs` |
| S6 | **One clock across the relay.** Every client sends the start budget on shared creates: web (fix `shared-decision.js:44` to use `api.js`'s header), Apple and Android (D2 (a), with their build bumps). The receiver forwards it as a header, **outside** the canonical request body, because the Source identity is the SHA-256 of the body (`shared_source_playback.rs:2109`). `source_start_budget_for_request` folds into D2's single create budget. | `shared_receiver_ingress.rs:208`; `source_actor.rs:113` |
| S7 | **A typed not-yet survives both hops.** The Source's typed refusal and `Retry-After` pass through `run_owner`, `actor_error` and the client unchanged. The receiver claim and the Source `SourceStartEntry` stay alive across a retryable answer for a bounded window, so the same `request_id` rejoins its work (D2 (c)'s durable identity on both hops). `shared-decision.js` returns the `streamFailure` shape the ladder reads. | `shared_receiver_playback.rs:538-549`, `:621-631`, `:1554-1557`; `shared-decision.js:14-29` |
| S8 | **The relay admits a growing rendition.** The receiver media actor stops retiring the whole session on a retryable Source resource answer (segment not yet, busy), and returns it to the client typed (`:2102-2111`, `:1236`). Start acceptance takes A1's first-publication contract instead of `vod && media_origin_ms == 0` (`:1602-1613`). `SharingHlsResource::parse` and `validate_sharing_playlist` admit A1 (i)'s separate immutable VOD playlist URI and any tag A1 adds (`plurx-core/src/sharing_resources.rs:191-215`, `:521-523`). | `shared_receiver_playback.rs:1963-2140` |
| S9 | **Cold resume through the relay.** The receiver carries a start position (`source_position_ms` is forced to 0 today, `shared_receiver_ingress.rs:202`). A range producer declares its origin, and the receiver checks that origin instead of zero. A1's "cold resume at 90 min within budget" test runs through the relay. | `shared_receiver_playback.rs:588`, `:1604` |
| S10 | **Abandonment is owned.** When the last receiver waiter leaves before publication, the receiver sends a bounded End and the Source settles the entry, freeing the registry slot and copy permit. A growing producer whose only viewer left finishes its index under the background budget, and Activity attributes it: what it is, why, and Stop, under the hardware-attribution rule. Every Source start ends with a logged terminal stage. | `shared_source_playback.rs:339`; `source_actor.rs:1613` |
| S11 | **Version skew is negotiated.** Source and receiver are independent installs (here `b08b5b8ec` against `09097650f`), and the peer start wire is closed (`closed_provided_fields`, `:2115-2134`). Apply the plan's H2 rule across the peer boundary: a sharing capability advertised in the handshake, so an old peer gets today's shapes, with old/new decode tests in both directions. | `plurx-core/src/sharing_receiver_sessions.rs:182-193` |
| S12 | **Locate asks only start-eligible peers** (§8). | `http/shared_source_forwarding.rs:1948` |

### 3.2 Non-goals

- **No longer client timeout for shared titles.** A 300 s spinner on a 4K
  title is the same failure with more waiting, and it would hide any later
  regression of the cause.
- **No shared-only rolling path.** A rolling Source producer next to A1 is
  the two-engine design A1 retires.
- **No pre-start subtitle work of any kind.** Not even "only the selected
  track": one 50 GB demux still misses every budget.
- **No change to pairing, grants or pinned TLS.** They worked end to end in
  this incident. Wire changes go through S11.

### 3.3 Tests and acceptance

**Tests (in A1's corpus):**

- **Start with a parked scan.** A real B-to-Source fixture
  (`shared_receiver_fixture.rs`) starts an unindexed copy. With the scan
  parked on `source_workers.index_hooks` (`source_actor.rs:1168`), the first
  playlist still publishes after the third segment. **Counterfactual:**
  restore the blocking pre-step and it never publishes. A fixture-sized
  file scans too fast to tell the two apart, so the hook is what makes this
  test meaningful.
- **Start with a parked subtitle extraction.** The same, with a text track
  and `native_hooks` (`:1247`) parked. The cue for t=0 is served from the
  windowed route.
- **DTS head.** A reordered-DTS-head HEVC fixture is indexed and played
  through the one engine. With `-xerror` restored, the index fails.
- **One recipe.** Local and Source entry points resolve the same `Recipe`
  and `SourceIdentity` for one file.
- **One clock.** A fake-clock test: the budget header crosses the relay, a
  Source "not yet" reaches the client typed, and the retry rejoins the same
  Source incarnation.
- **Relay.** A growing rendition with a segment-not-yet survives at the
  receiver. A cold resume at 90 min works through the relay.
- **Abandonment.** The last waiter leaving before publication frees the
  Source slot, and the background finish is listed in Activity.
- **Skew.** An old receiver against a new Source, and the reverse, both
  either play or get today's typed refusal.

**Acceptance (fleet, after the owner deploys):** on the two-account setup,
title A, title B and a second reordered-DTS-head title each reach first frame
within the A1 remux bound (≤ 5 s share of eligible attempts) from cold. A
second shared viewer on a different owner uses the first play's cluster
index. The receiver logs no `revoked` start for a viewer who is still
waiting.

## 4. Predictive preparation fails while any shared session is active

**Symptom:** every minute, lab6 logs `predictive preparation deferred
error=database error: invalid background job row: invalid type: null,
expected i64`. It started at 17:24:34 with the Source session and stopped
when that session's lease expired. No predictions are made for anyone in
that time.

**Root cause:** the `viewers` CTE in
[`demands`](../../crates/plurx-core/src/store/background_jobs_preparation.rs)
(`plurx-core/src/store/background_jobs_preparation.rs:55-60`) reads every
active `media_sessions` row. A Source session has `principal_kind =
'sharing'` and `user_id = NULL`; the Store held one, against 19 `local`
rows. That row decodes as `PreparationDemand::Viewer { user_id: i64 }` and
fails, and `rows.iter().map(decode).collect()` loses the whole tick. The CTE
also reads `$.request.file_id`, a path the Source recipe does not have.

**Sibling defect, the same cause:** the replica-equality dump decodes
`user_id: i64` for `MediaSessionDumpRow`, `MediaSessionRequestDumpRow`,
`MediaPlaybackPointerDumpRow` and `MediaSessionPreparationDumpRow`
(`plurx-core/src/store/hiqlite.rs:7714-7760`). Their SELECTs have no state
or principal filter (`:5344-5350`, `:5442-5452`). Replica validation
therefore fails whenever any sharing row exists, in any state.

**Fix:**

- Filter `viewers` to `principal_kind = 'local'`. Viewer demand means "this
  account's next episode", and a sharing principal is not an account here.
- Make the four dump rows `user_id: Option<i64>`: those rows are
  legitimately principal-less.
- Audit every principal-keyed table read (`media_sessions`,
  `media_session_requests`, `media_playback_pointers`,
  `media_session_preparations`) for non-null decoding. Equality joins on
  `user_id` are already NULL-safe (`background_jobs.rs:2054`,
  `background_jobs_viewer_analysis.sql:232`).

**Tests:**

- A store contract test (both backends) with one active sharing session and
  one local one returns exactly the local viewer's demand.
- A replica-dump test with a sharing row passes.
- **Counterfactual:** both tests fail on today's code with today's error.

## 5. One background source reader on lab3 can never be recovered automatically

**Symptom:** all four nodes log `Shared source reads need recovery; affected
media jobs cannot start`. `plurx_background_source_recovery_pending` is 1,
and since the sample is cluster-wide, every node reports the one record.

**Evidence:**

- `background_source_custody` holds custody `16da3798…`, phase `active`. It
  belongs to a `dv_title_scan` job (`a68e6605…`, file 6323, 68 GB) owned by
  lab3's previous process boot.
- The job is still `running`, with its lease expired at 04:35 UTC.
  `source_io_occupancy` still charges the slot, 13 h later.
- lab3's journal holds the activation at
  `source-custody-recovery/v1/background-16da3798….json`, with
  `"machine_id":"","boot_id":""`.

**Root cause:** `write_active_at` deliberately writes an empty identity
when `Identity::current()` is `None` (`source_custody/recovery.rs:315-318`).
That happens in a container without the identity binds, and it is
expected, because the v2 reader witness is the designed recovery path.
This activation came from a build that wrote neither: no v1 identity and no
v2 witness. `verified` (`:462`) needs a valid prior identity and a
different current boot, so it can never pass, and lab3's host has not
rebooted since 2026-10-12. This is the finite legacy case that
[SOURCE-CUSTODY-RECOVERY.md §1.2](../server/SOURCE-CUSTODY-RECOVERY.md)
handles with host-admin physical-stop proof, and that proof was never run
on lab3: `/run/plurx-physical-stop` does not exist there, and the container
has no bind for it.

**The remedy's preconditions were checked on lab3 (2026-11-08):**

| Check | Result |
|---|---|
| Journal file SHA-256 vs `ACTIVATION` (`physical_stop.rs:28`) | `50f069fb…` — matches |
| Job payload scanner digest vs `TITLE_SCANNER` (`:38`) | `12315d1f…` — matches |
| Daemon euid / capabilities (`:204-213`) | uid 1000; `CapEff` / `CapPrm` 0 — accepted |

**Fix:** no code change. This is the designed proof of a physical stop, not
a release on a guess. Run the helper on lab3 as root, following §1.2's
procedure:

- `scripts/source-custody-reader-stop --check` with its required
  `--host-enrollment` and `--reference-manifest`, then `--publish`;
- add the read-only `/run/plurx-physical-stop` bind to lab3's Compose
  override in the deployment playbooks;
- redeploy lab3.

The daemon then settles the custody, clears its journal and frees the slot.
This is the owner's step (deploys).

**Prevention:** keep recovery platform- and backend-specific. The lab3 owner
still uses the exact host-admin stop proof above. For replicated Linux startup,
PR #1570 requires actual-host machine and boot identity binds and checks the
bound boot against the running kernel before the daemon serves. Native macOS
recovery and non-authoritative SQLite startup remain unchanged; see the
[startup contract in the recovery ledger §1.5](../server/SOURCE-CUSTODY-RECOVERY.md#15-linux-replicated-startup-requires-actual-host-identity).

### 5.1 Missing host identity let a replicated Linux daemon start without reboot recovery

On 2026-11-13, all four replicas had the same `2b6f4c0e…` custody for
`transcode_prepare` job `49dd5948…` (file 104, recent-demand preparation).
The job was cancelled while custody remained active without settlement. Its
anchor was invalidated by a raw launch at 02:30:05 UTC; Docker records daemon
replacement at 02:30:23–24 UTC, and the host rebooted at 03:54:57 UTC. The v2
record's kernel UUID is now historical and it has no namespace field. The v1
machine/boot IDs are empty because the deployment lacked both actual-host
identity binds. Cancelling the job, replacing the daemon, or observing a later
host reboot did not itself settle that custody. This is a separate owner from
the resolved lab3 DV migration above; the exact child executable was not
recorded.

Merged PR #1567's existing exact-custody census recovery released this owner
at 2026-11-13 19:01:59.868984 UTC. Managed sidecars were clean; the daemon
recorded the normal release acknowledgement before the temporary root proof
was removed, and the census binding remains installed. No direct database
edits were made. Read-only checks at 19:06:45 UTC found the exact custody row
absent and `plurx_background_source_recovery_pending=0` on lab3, lab4, lab6
and media1, all at revision
`882d4b691e048f24b6c3e665717aa5966db71a28`. This verifies only that owner on
those four replicas.

The missing prevention was a serving-boundary check: replicated Linux could
start without machine and boot IDs tied to the actual host and current kernel.
PR #1570 adds that fail-closed startup requirement and the mandatory read-only
Compose/systemd mounts. The four audited hosts had valid matching mount inputs
at 19:03:52 UTC, but their running revision 882 does not include the startup
guard; rollout remains pending. The historical owner was recovered through
the existing [exact-custody census path in ledger §1.3](../server/SOURCE-CUSTODY-RECOVERY.md#13-a-dead-owners-poisoned-witness-uses-a-host-source-open-census).
No alternate recovery path was added. Broader fleet and playback acceptance
remain open.

## 6. Every node warns about two removed Raft members, every minute

**Symptom:** lab6 logs `hiqlite::split_brain_check: … sqlite node 3 not in
membership config … [1, 5, 6, 7]` for nodes 3 and 4, six lines a minute
(360 an hour).

**Root cause:**

- Each node's `membership.json` keeps the `bootstrap` list from the moment
  it joined: lab6 `[1, 3, 4]`, lab4 `[1, 4, 5]`, lab3 `[1, 5, 6]`, media1
  `[1]`.
- Raft ids 3 and 4 were the earlier lab4 and lab3 identities, removed on
  2026-08-25 and 2026-09-02. Ids 6 and 7 now hold those same addresses.
- [`migration.rs`](../../crates/plurx-core/src/cluster/migration.rs)
  (`:3757-3764`) passes that list to hiqlite as its static `nodes`. The code
  says itself that the file "records the bootstrap list once and is never
  refreshed" (`:1634`).
- hiqlite copies `nodes` into `split_brain_check` at start
  (`vendor/hiqlite/src/start.rs:260-265`) and compares them with live
  membership every 60 s.

An initialized node does not dial this list to rejoin
(`vendor/hiqlite/src/init.rs:244-273`). Only the check uses it, plus an
uninitialized node that has lost its volume. The defect is therefore a
false alarm every minute plus a wrong lost-volume bootstrap, not a
replication risk.

**Fix:** derive hiqlite's `nodes` before start from the committed
membership in the closed state machine: `cluster_nodes` with `removed_at IS
NULL`, read the way `admitted_peer_count` already reads it (`:1636`). Then
rewrite `membership.json`'s `bootstrap` from the same set, so a lost-volume
start also sees current peers. Rewriting the file only after the Store opens
would leave the warnings running for the rest of that boot.

**Test:** a migration test that removes a peer and restarts: hiqlite's
`nodes` and the file both omit the removed id. A second test starts with a
stale file and must behave as today. **Acceptance:** zero split-brain
warnings after one restart per node.

## 7. The leader intermittently cannot confirm leadership for reads

**Symptom:** media1, the Raft leader, logs `timeout while confirming
leadership for read request target=5 … target=6` at the same millisecond
for both followers, about 28 times in the 40 minutes after the 17:00
deploy. Followers then log `transient replicated authority read failed;
retrying retry=QuorumUnavailable`, and one such retry was a user-facing
`/api/v1/dvr/overview`.

**What was measured, and what it rules out:**

| Hypothesis | Measurement (2026-11-08) | Verdict |
|---|---|---|
| Network path | RTT media1→lab6 0.64 ms avg, 3.8 ms max over 300 pings, 0 % loss; same L2 segment | ruled out |
| RX overruns on media1's NIC (`igc`, RX ring 256 of 4,096) | 0.5 s counter sampler, 17:47:48–18:27:48 UTC, 4,662 samples. Bursts of ≥ 20 overruns per 0.5 s occur in 8.9 % of samples. In the 2 s before each read-confirm event: 18:23:53 → 166 overruns, 18:25:25 → 330, 18:25:56 → 258. The fourth event, a lone `Elapsed` at 18:12:32 with no read-confirm pair, saw 0. | **cause of the read-confirm timeouts** — 3 of 3 coincide with bursts (≈ 0.07 % by chance) |
| Follower durable writes | 20 s fsync probe on `/srv/plurx`: lab6 p50 2.9 / p99 5.2 ms · lab4 p50 8.9 / p99 80 ms (disk 92 % full) · media1 p50 1.3 / p99 2.6 ms | not the cause: well inside 800 ms, and lab6 is fast |

**Root cause:** media1 serves media and reads its library over NFS through
one 2.5 GbE `igc` port whose RX ring is 256 descriptors. Inbound bursts
overflow the ring, and the NIC drops whatever arrives during the overflow,
including the followers' Raft replies. The followers then retransmit after
an RTO of at least 200 ms. Those retransmits are counted on the followers,
which is why media1's own `TcpRetransSegs` stays flat. Two losses on one
flow are enough to push a read confirmation past the 800 ms window
(`HIQLITE_HEARTBEAT_INTERVAL_MS`, `plurx-core/src/cluster/migration.rs:151-160`).
It hits both followers at once because both replies arrive inside the same
overflow. An early draft read the flat retransmit counter as ruling the NIC
out. That was wrong: the drops are inbound, so the retransmits happen on the
other side.

**Fix:**

1. **Size the ring for the load (owner, host role in the deployment
   playbooks):** `ethtool -G enp86s0 rx 4096` on media1, persisted. Check the
   other nodes' rings the same way. Then confirm adaptive interrupt moderation
   and RSS spreading with `ethtool -c` and `-l`. Acceptance: `missed` stops
   rising during playback, and no read-confirm timeouts over 24 h of normal
   load.
   **Built 2026-11-12** in the private host baseline. Each physical uplink
   port is raised to its pre-set maximum once, and a `.link` file persists it
   (`RxBufferSize=max`, applied when the device appears, before link-up). The
   audit then fails until every uplink reads its maximum. A read-only audit
   that day found four of the six hosts at 256 of 4,096. The media1 port had
   been set to 4,096 by hand and would have reverted on reboot. One node's NIC
   tops out at 256 in hardware. Applying it is the owner's deploy step;
   acceptance above still applies.
2. **Make the next one attributable (code, small):** the vendored
   `raft_client` timeout path (`vendor/hiqlite/src/network/raft_client.rs:1399`)
   warns with the target, request kind, entry count and payload bytes. The
   lone 18:12:32 `Elapsed` cannot be attributed today. If it recurs with a
   large payload, the next step is to give heartbeats their own lane rather
   than queue them behind bulk appends.
3. **Do not raise the heartbeat or election windows.** That hides packet
   loss instead of removing it, and 800 ms already carries the
   rolling-update safety argument at `:151-158`.

## 8. Source locate is refused by the learner on every start

**Symptom:** lab3 logs `http request failed status=503 latency_ms=0
path=/internal/cluster/sharing/source/locate` for every shared start.

**Root cause:** lab3 is a learner (raft id 7). `capacity_role_refusal`
(`http/mod.rs:2850`) answers every route outside `learner_route_eligible`
(`:2665`) with `learner_route_ineligible`. `collect_fresh_location`
(`http/shared_source_forwarding.rs:1948`) fans out to every reachable
`media_peers()` member and never checks whether that member's role admits
the route.

**Fix:** filter locate's peers with the same predicate:
`learner_route_eligible(POST, LOCATE_PATH)` applied to each peer's committed
role. `ActivityPeer` carries no role (`plurx-core/src/cluster/membership.rs:2316-2321`),
so take the role from `cluster_nodes`. Using the one predicate means the
two checks cannot drift apart.

Locality was checked: lab3 mounts the same five library paths as lab6, so
no file is present only on lab3, and excluding the learner loses no title.
Whether learners should *serve* Source media, the way they serve node-local
media routes, would be a deliberate widening of `learner_route_eligible`
for A1's design review. It is not something to add through this fan-out.

**Sibling erasures, fixed in the same PR:** `credit()`
(`http/shared_source_forwarding.rs:45-68`) and `PLACEMENT_IO`
(`http/shared_source_ingress.rs:1042-1044`) turn a full semaphore into
503. They should answer typed 429 with `Retry-After`, which S7 then carries
to the client.

**Test:** a forwarding unit test with one learner and one voter peer: only
the voter receives locate. **Counterfactual:** today's code sends both.

## 9. Shared posters fail on first view

**Symptom:** maca logged 18 failed `/api/v1/shared/imports/…/art/…`
requests. Each was a different image that failed exactly once, in two
browse bursts (six in 12 s at 17:07; twelve in 29 s at 17:29–17:30). Each
took 290–630 ms, so every one reached the Source and came back refused.

**Most likely cause:** a size the Source has not rendered yet. The Source
answers `429 sharing_art_variant_busy` whenever the W300/W500/W780
derivative a page asks for does not exist yet. It enqueues the render and
refuses without `Retry-After` (`http/images.rs:319-341`,
`http/shared_artwork.rs:185`). "Each image failed once" is what that cause
looks like.

Contention is less likely. The receiver caps itself at 4 operations
(`RECEIVER_OPERATIONS`, `:23-24`), so the Source never sees more than 4
concurrent requests from it, the same as `SOURCE_OPERATIONS`. The receiver
erases the code: `read_artwork(…).map_err(|_| unavailable())` (`:240-242`)
turns every peer refusal into 503. That is why the cause cannot be read
from maca's log, and it has to be fixed whichever cause it was.

The receiver also never reads its own snapshot. `publish_disk_snapshot`
(`:381`) writes one, but `receiver` always calls the peer
(`sharing.rs:1807-1866`). Responses are `private, no-store` (`:159-162`),
so every grid render refetches every poster across the tailnet.

**Fix:**

- **Carry the code.** The receiver passes the Source's typed refusal through
  as 429 with `Retry-After`, never 503, and logs it.
- **Answer variant-busy as a scheduled not-yet.** `Retry-After` comes from
  the queued render job, and the shared artwork loader retries once after
  it.
- **Serve the snapshot first.** The receiver serves its verified snapshot
  before dialling the peer, revalidating against the catalogue revision it
  already carries.

**Tests:**

- First request for an unrendered variant returns 429 with `Retry-After`,
  and the retry succeeds once the render lands.
- A forced Source 429 reaches the client as 429.
- A second view of an image is served from the snapshot, with zero peer
  calls.
- **Counterfactual:** each test fails on today's code.

## 10. Order of work and owners

| Item | Lane | Size | Blocked on |
|---|---|---|---|
| §3 S1–S12 | A1 (design review with B1; then Opus build) | adds ~L to A1's XL | A1 design review |
| §4 NULL principal (both sites) | standalone PR | S | — |
| §6 membership from committed state | standalone PR | M | — |
| §7 step 1, media1 RX ring | built in the host baseline 2026-11-12; owner applies | ops | owner apply |
| §7 step 2, RPC attribution | done: timeouts name node, kind, entries and bytes | S | — |
| §8 locate filter and typed 429s | standalone PR (S12 reuses it) | S | — |
| §9 shared art | standalone PR | S–M | — |
| §5 lab3 custody | resolved: `background_source_recovery_pending` 0 on every node, 2026-11-12 | ops | — |

The standalone PRs touch disjoint files, so they can stack into one
fast-lane run. The PLAYBACK-START-PLAN A1 row links §3.

## 11. Owner decisions — accepted 2026-11-08

1. **Fallback after S4:** no transcode fallback. A1 owns the remaining copy-engine limitations; hardware use is not silently introduced.
2. **Poster variant still rendering:** keep the typed retry; do not serve the original as an interim image.
3. **Learners as Source owners:** excluded from Source start. Any wider serving role belongs in A1's design review.

The owner accepted all three recommendations in the handoff. On resumption,
Codex also retained the live scope check for cached posters: tight revocation
wins over a TTL proof cache. A snapshot avoids fetching the image body again;
it does not remove the authorization round trip. This qualifies §9's original
zero-peer-call target.

## 12. Evidence ledger

| Date | Evidence | Scope |
|---|---|---|
| 2026-11-08 | Receiver and source logs; both Stores (read-only snapshot copies, removed afterwards); the four membership files; lab3's recovery journal and digests; container uid and capabilities | §1–§9 |
| 2026-11-08 | Title B, 20 s copy with and without `-xerror` (read-only, output to `/dev/null`) | §2.3 |
| 2026-11-08 | Ping, NIC counter sampler (media1, 17:47:48–18:27:48 UTC), fsync probes on three nodes | §7 |
| 2026-11-08 | The sampler log and probe files were the only things written on any node, and all were removed. No setting or service was changed. | — |

## 13. Review ledger

| Date | Reviewer | Result |
|---|---|---|
| 2026-11-08 | Opus 5.5, adversarial, at `3d3d7b62a` (Fable 5.1 was out of usage credits; the owner chose Opus) | 7 P1 · 15 P2 · 2 P3. All applied: one recipe and one index (S1–S3); the 35 s clock; retry across the relay (S7); the relay hazards (S8); version skew (S11); resume (S9); abandonment (S10); §9 cause re-attributed; §5 preconditions checked; §6 fix moved before start; §7 alternatives measured, and the cause found by correlation; S4 re-grounded on the measured `-xerror` cause; citations corrected |
