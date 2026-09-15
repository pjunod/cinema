# Live TV DVR and reminders — proposal and implementation plan

**Status:** ready to build (v2 — Astra's review of 2026-09-13 addressed, §10) ·
**Executes:** the recommended options in
[LIVE-TV-DVR-AND-REMINDERS-OPTIONS.md](LIVE-TV-DVR-AND-REMINDERS-OPTIONS.md)
§5, which Paul accepted 2026-09-13 · **Verified against:** `main` `311683bc` ·
**Written:** 2026-09-13, revised the same day · **Lane:** `effort/live-tv-dvr` ·
**Builder:** Opus

Companion to the options doc (why these choices), to
[LIVE-TV-GUIDE-AND-UI-PLAN.md](LIVE-TV-GUIDE-AND-UI-PLAN.md) (the page this
lives on), to [HDHOMERUN-LIVE-TV-STATUS.md](HDHOMERUN-LIVE-TV-STATUS.md) (the
tuner engine as proved on hardware) and to
[../DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (how each PR here
reaches `main`). This is *the executable plan*: exact contracts copied from
the tree, milestones with a runnable acceptance check each, and the
guardrails an executing agent must not cross.

**How to work it.** Read §1–§4 once. Build milestone by milestone (§6), each
as one task PR into `effort/live-tv-dvr`: proper commits · the fast local
lane only · open as a draft (`WIP:` title) · exactly one adversarial review ·
implement the findings · mark ready, which is what runs the fast lane ·
merge. Every
`path:line` here was read at `311683bc`; **re-verify against the file at
build time** — `live_tv.rs` moves every week. If a step seems to require
changing something §5 forbids, stop and flag it in the PR instead of doing
it.

**Review status.** Astra's adversarial review (2026-09-13, eight P1 and
three P2 findings) is answered finding by finding in §10; every accepted
change is already folded into the sections it names, so a builder reads
§3–§6 as the contract and §10 only to see why. The reviewer's own
regression cases are the acceptance evidence of M2 and M4 — they are
tests to write, not tests already run.

---

## 1. Objective

Record from the HDHomeRun into a library that every client plays, and tell
the viewer when a marked programme is about to start. Concretely, when this
is done:

- Any guide cell on web, Apple and Android offers **Record · Record series ·
  Remind me** beside Watch; a Recordings segment lists the library, the
  schedule (with conflicts and why) and the rules.
- The owner node records by writing the tuner's transport stream to disk
  under the DVR root, with a sidecar of guide metadata; the file appears in
  a `recordings` library and plays through the finite-media path on every
  node, converted per player exactly as Original Quality decides.
- A recording is an Activity row with a reason and a Stop; nothing holds a
  tuner or writes to disk without saying so in the product.
- A reminder fires as an in-app overlay on every open client, as a local
  notification on phones with the app closed, and as one webhook POST.
- Nothing is gated. `dvr.enabled` is a switch; the Developer tab says what
  should be true first and whether it is.

---

## 2. The rulings this plan executes

From the options doc §5, all accepted 2026-09-13:

| # | Decision | Ruling |
|---|---|---|
| 1 | Guide horizon | **G2 + G3** — HDHomeRun DVR subscription paging *and* XMLTV; the free 4-hour tier stays the floor |
| 2 | Series matching | **S3** — `series_id` when the guide carries one, normalised title + channel lock otherwise; the mode is stored on the rule and shown |
| 3 | Tuners | **C2 + C3** — recordings may hold `max_sessions − reserve` slots (reserve default 1); a viewer refused by `tuner_capacity` is offered the recordings holding tuners and may stop one, attributed |
| 4 | Capture | **K1** — the tuner's bytes go to a `.ts` file untouched; play-while-recording is a later milestone |
| 5 | Library | **L1** — a `recordings` library kind, disk is truth, sidecar JSON is the metadata; "file into Shows" later |
| 6 | Reminders | **R1 + R2 + R3** — in-app overlay, phone local notifications, outbound webhook; APNs later |
| 7 | UI | Record is one press, series is two; marks on cells are 12 px on TV / 8 px on web, no text |
| 8 | Webhook policy | **open** — `https` anywhere, or `http` to RFC 1918 only (§3.8); a weaker boundary than any outbound call plurxd makes today |

Two rulings from the same day's reliability work bind this plan too
(`LIVE-TV-GUIDE-AND-START-RELIABILITY.md`, PR #273): the guide is cached
**node-locally on the owner under `cache_dir/live-tv/guide.json`, never in
the replicated settings table**; and *a possibly-held tuner is never a
reason to refuse a viewer* — the owner evicts a stray before answering
`tuner_capacity`. §3.4 is written to those. The dependency is pinned by
commit, not by effort (§6, "Ordering against the reliability effort"):
guide persistence (reliability **M1**) must be on `main` before DVR M0
opens; the same-viewer stray eviction (reliability **M3**) must be on
`main` before DVR M2 promotes. No DVR PR is based on
`effort/live-tv-reliability`, so promoting the DVR lane can never drag
that effort's unmerged work into `main`.

---

## 3. Design

### 3.1 One picture

```
                        owner node (config.owner_node_id)                     every node
 ┌──────────────────────────────────────────────────────────────┐   ┌──────────────────────┐
 │ guide_refresh_loop ──▶ guide.json (cache_dir/live-tv, #273)  │   │  Store (replicated)  │
 │        │  + series_id/programme_id, horizon ≤ 336 h  (M0)   │   │  dvr_rules           │
 │        ▼                                                     │   │  dvr_recordings  ◀───┼── UI on any node
 │ dvr_loop (15 s)  rules × guide ──▶ dvr_recordings (M2)      │──▶│  dvr_reminders       │
 │        │   conflicts by priority · reserve · disk floor      │   └──────────────────────┘
 │        ▼                                                     │
 │ start capture: registry slot ──▶ GET /auto/v{n} ──▶ file.ts │   recordings library (M3)
 │        │        + prefix probe ──▶ file.json sidecar         │   scan ──▶ items/files ──▶ VOD path
 │        ▼                                                     │
 │ Activity row "record" · DELETE /dvr/recordings/{id} = Stop  │   reminders: overlay · phone local · webhook (M4)
 └──────────────────────────────────────────────────────────────┘
```

The owner node is the only process that touches the tuner today
(`LiveTvConfig.owner_node_id`, `live_tv.rs:148`; `owner_start`,
`http/live_tv.rs:646`). The DVR keeps that: scheduling and capture run in a
loop on the owner, like `guide_refresh_loop` (`live_tv.rs:3206`) and
`scratch_sweep_loop` (`live_tv.rs:2750`). What every node needs — the
rules, the schedule, the recordings, the reminders — lives in the
replicated Store, so the UI on lab4 shows what media1 is recording.

### 3.2 Guide identity and horizon (M0)

`LiveTvProgramme` (`live_tv/guide.rs:101–118`) gains two nullable fields:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub(crate) series_id: Option<String>,      // HDHomeRun SeriesID · XMLTV dd_progid series part
#[serde(default, skip_serializing_if = "Option::is_none")]
pub(crate) programme_id: Option<String>,   // HDHomeRun ProgramID · XMLTV dd_progid
```

`HdhrGuideEntry` (`guide.rs:349–361`) parses `SeriesID` and `ProgramID` —
**with explicit `#[serde(rename = "SeriesID")]` / `"ProgramID"`**, because
the struct's `rename_all = "PascalCase"` would look for `SeriesId` and
parse nothing, which is exactly why `ImageURL` (`:358`) and
`OriginalAirdate` (`:356`) carry explicit renames today. `parse_xmltv` (`guide.rs:626`) reads
`<episode-num system="dd_progid">EP012345670023</episode-num>`: the first
10 characters are the series id (`EP01234567`), the whole string the
programme id; `xmltv_ns` keeps producing the `SxxEyy` string it does today
(`xmltv_ns_episode`, `guide.rs:913`). Both bounded by
`bounded_text(..., 64)`.

Horizon: `guide_hours: u8` → `u16` in `LiveTvConfig` (`live_tv.rs:167`)
and in the settings field (`live_tv_guide_hours: Option<u8>`,
`system.rs:2172`); `MAX_GUIDE_HOURS` 72 → **336** (`live_tv.rs:73`);
`MAX_GUIDE_REQUEST_HOURS: u8 = 72` (`http/live_tv.rs:32`, consumed by
`requested_window` at `:89–99`) → `u16`, 336; the same bound in
`validate_guide` (`live_tv.rs:296`), the `docs/API.md:1825` row
(`hours=<1..72>` → `<1..336>`), and the Apple/Android settings validators.
The setting rides the existing generation CAS unchanged (`update_settings`,
`system.rs:2405`).

Two caps that would silently defeat a 14-day guide, both raised in M0:
`GUIDE_MAX_PROGRAMMES_PER_CHANNEL = 200` (`guide.rs:48`, enforced in
`normalise_programmes` at `:250`) → **800** (a fortnight of half-hour
slots is ~670), and `normalise_programmes` itself (`guide.rs:234–256`),
which today resolves an overlap by moving the *next* row's `start` to the
previous row's `end` — and `start` is the DVR's identity (§3.3). M0 makes
it shorten the previous row's `end` instead, so a start never moves, and
adds a de-duplication pass keyed on `(start, title)` before the sort
(keep the row with ids, else the later `end`). `merge_channel`
(`guide.rs:511–521`) concatenates and re-normalises today with no
per-programme merge; the de-dup pass is where a cached programme without
ids picks them up from a fresher page — new code, not existing behaviour.

Fetching a fortnight is not one refresh. `fetch_hdhomerun_guide`
(`live_tv.rs:3016`) already extends per channel with `Start=` pages under
`GUIDE_MAX_EXTENSION_REQUESTS = 64` (`guide.rs:50`, loop at
`live_tv.rs:3056`) and a 25 s `GUIDE_REFRESH_TIMEOUT` (`guide.rs:44`). M0
makes the extension **incremental against the persisted cache**: a tick
re-fetches the bulk window, then extends only channels whose cached tail
ends before `now + guide_hours`, spending at most 64 requests per tick and
keeping whatever it got. A 12-channel, 14-day guide fills over a few ticks
and stays full thereafter; a free-tier tuner answers nothing past ~4 h and
the loop stops asking (the existing "empty or repeated answer stops that
channel" rule, `live_tv.rs:3052–3055`). Filled is not the same as
current: each tick also **revalidates one day** of the cached horizon,
cycling through the fortnight, so a provider's schedule change reaches the
cache within 14 ticks (~5 h) and reconciliation (§3.3) can act on it;
the request budget is shared between extension and revalidation, tail
first. This needs the cache to survive
between ticks, which is exactly what PR #273's `guide.json` provides — **M0
is built on `effort/live-tv-reliability` after #273's M0–M2 land, or on
`main` once they are there**; it does not reimplement persistence.

What is deliberately not verified here: whether the subscription tier
answers `Start=` days out and carries `SeriesID`. §8 has the hardware
prompt; M0's acceptance is written so the code is correct either way.

### 3.3 Rules, airings, identity (M1, M2)

A **rule** is `(match_mode, match_value, channel_id?, new_only, keep,
padding, priority)`. A rule matches a guide programme when:

```
 match_mode = series_id  ──▶ programme.series_id == match_value
 match_mode = title      ──▶ normalise(programme.title) == match_value
                              AND (rule.channel_id IS NULL OR channel matches)
 normalise(t) = lowercase · NFKC · collapse whitespace · strip trailing " (YYYY)"
```

*Record series* from a cell creates the rule with `series_id` when the
programme has one, else `title` + the cell's channel — and stores which,
so the UI can say "title match · 7.1 only". `new_only` means
`original_air_date` is within 7 days before the airing's start, or the
programme carries an XMLTV `<new/>` (M0 adds `is_new: Option<bool>` to
`LiveTvProgramme` for XMLTV only; HDHomeRun's `Filter` tags never say
"new" reliably, so they are not consulted).

An **airing** is identified by `(channel_id, airing_start)`. The unique
index in §4.2 covers **every** state, so one airing is one row for its
whole life: a one-off request, four rules that all match it, and the
user's later decisions all land on that row. Expansion is an
insert-if-absent; it never resurrects a row the user cancelled (§10 #2).

**Row origin and ownership.** Each row carries `origin` (`manual` or
`rule`) and, for rule rows, `rule_id`. Every tick, for each *pending*
row (`scheduled` or `conflict`):

```
 origin = manual ──▶ untouched by any rule edit; only the requester (or an admin) changes it
 origin = rule   ──▶ rule_id re-pointed to the highest-priority ENABLED rule that still matches
                     no enabled rule matches any more ──▶ state = withdrawn, reason = "rule disabled|edited|deleted"
                     (withdrawn is not cancelled: expansion re-materialises it if a rule matches again)
 the guide no longer has a programme at (channel_id, airing_start) with this title
                 ──▶ rule rows: withdrawn, reason "guide changed"
                 ──▶ manual rows: state = stale, reason "programme moved or renamed" (the UI offers
                     "record the new time?", which is a new POST for the new airing)
```

Rows that are `recording`, `done`, `partial`, `failed`, `missed`,
`cancelled` or `deleted` are never touched by reconciliation. Cancelling
is a user decision and stays: `cancelled` rows are skipped by expansion
for good, and `POST /dvr/recordings/{id}/restore` is the only way back to
`scheduled` (§4.3).

**Padding** is `pad_start_s` / `pad_end_s` on the rule (defaults from
`dvr.pad_start_s` = 60, `dvr.pad_end_s` = 120); `capture_start =
airing_start − pad_start`, `capture_end = airing_end + pad_end`. A
one-off from a cell copies the defaults. Overlapping captures on the *same*
channel (the 8:30 show's tail pad and the 9:00 show's head pad) share one
tuner slot: the engine records back-to-back airings on one channel as
**one tuner GET, two files** — §3.5.

### 3.4 Tuners: admission with a reserve

Viewer admission today: `start_local_inner` refuses with
`LiveTvError::Capacity` (wire `tuner_capacity`, `live_tv.rs:790`) when
`registry.sessions.len() >= max_sessions` (`live_tv.rs:2200–2205`), and
a configuration whose `max_sessions` exceeds `tuner_count.min(4)` is
refused at lineup time (`InvalidConfig`, `live_tv.rs:1930–1935`) — refused,
not lowered, so the arithmetic below reads a validated `max_sessions`.
`LiveTvRegistry` (`live_tv.rs:1182`) holds
`sessions: HashMap<String, Arc<LiveTvSession>>`.

M2 adds `transports: HashMap<String, Arc<DvrTransport>>` — **one entry per
channel being recorded, however many recordings share it** (§3.5) — and
two rules evaluated under the same registry lock:

```rust
fn held(&self) -> usize { self.sessions.len() + self.transports.len() }

// viewer:    admitted when held() < max_sessions
// recording (a NEW transport; a sink joining an existing transport needs no slot):
//            admitted when held() < max_sessions
//                 AND transports.len() < max_sessions - reserve      // reserve = dvr.tuner_reserve
```

Both conditions are about occupancy, not order: with four tuners and
`reserve = 1`, recordings may hold at most three transports, and whether
the viewer arrived first or last, "one viewer + three recordings" is
admitted and "four recordings" is not (§10 #3). The `max_sessions −
reserve` cap is what the scheduler (§3.7) plans against; the `held()`
check is what protects a viewer who is already watching.

A viewer refused while transports hold slots gets the existing
`tuner_capacity` code **plus** a new field on the typed error body:

```json
"holders": [{"channel_id": "…", "guide_number": "5.1", "channel_name": "WNYW",
             "sinks": [{"recording_id": "…", "title": "Sports Tonight", "ends_at": 1789003200}]}]
```

The client offers *Stop "Sports Tonight" and watch?* only for a transport
with **one** sink; a transport with two sinks is listed as "recording
Sports Tonight and Postgame Desk until 9:02" with no single-stop offer,
because stopping one of them frees nothing (§10 #4). The confirm calls
`DELETE /dvr/recordings/{id}`, which answers `{pending: true}` and is
consumed by the owner's tick (§3.7 step 4, §10 #8); the client then polls
`GET /dvr/recordings/{id}` until the state leaves `recording` and **only
then** retries the start — capacity is released at cleanup, not at
request. The #273 ruling — evict the viewer's own stray before answering
capacity — applies before this arithmetic and is untouched.

Recording-vs-recording: the scheduler walks the next 14 days in
`capture_start` order, allocating `max_sessions − reserve` virtual
transports by rule priority (lower `priority` wins; `manual` rows rank
above every rule), with back-to-back airings on one channel costing one
transport. An airing that finds no transport is written `conflict` with
`state_reason = 'no tuner (1 reserved for viewing)'` and stays in the
schedule so the UI can show it and offer *Move rule up* / *Skip* (Skip =
`cancelled`, durable). The allocation is recomputed every tick, so freeing
a transport un-conflicts the next airing automatically — subject to the
late-start rule in §3.7.

### 3.5 Capture: one transport per channel, one sink per recording (M2)

The live path opens exactly one GET per session (`open_tuner_stream`,
`live_tv.rs:4035`, enforced by
`one_tuner_get_runs_the_full_hls_lifecycle_and_stop_waits_for_cleanup`,
`live_tv.rs:6113`), keeps an 8 MiB / 3 s prefix (`collect_live_prefix`,
`live_tv.rs:4074`; `SOURCE_PREFIX_BYTES` `:112`), probes it with ffprobe
(`probe_live_source`, `:4247`) and pumps the response body into FFmpeg's
stdin (`pump_tuner_stream`, `:4693`) — FFmpeg's `-i` is `pipe:0`
(`live_ffmpeg_command`, `:4416`), not the tuner URL.

A **transport** is the same open and the same prefix probe with the pump's
sink replaced by a fan-out to files; a **sink** is one recording's file
with its own `[capture_start, capture_end)` window and byte count:

```
 DvrTransport { channel, generation, owner_serving_generation, cancel, delivered, sinks: Mutex<Vec<DvrSink>> }
   └▶ open_tuner_stream(channel)            same function, same 404/503 mapping (:4050–4062)
        └▶ collect_live_prefix              same prefix, so every sidecar gets the same facts
             ├▶ probe_live_source ──▶ LiveSourceFacts ──▶ each sink's sidecar.json
             └▶ pump_tuner_fanout(input, transport, cancel)     new: for each chunk, write to every
                                                                 sink whose window contains now
                       DvrSink { recording_id, window, file: File (O_EXCL), bytes: AtomicU64, cancel }
```

No FFmpeg process. HDHomeRun's `/auto/v{n}` is a single-program MPEG-TS
and Silicondust documents saving the HTTP stream straight to a file; the
finite-media path already opens a `.ts`. Playability of *our* files
through *our* path is still M2's acceptance (§10, "raw TS"): play one
sole-sink capture and the **second file of a shared transport** through
`/api/v1/decision` and a session on the lab Mac. If either fails, the
fallback is `ffmpeg -i pipe:0 -map 0 -c copy -f mpegts` per sink — the
same fan-out with the existing `live_ffmpeg_command` skeleton and no
encoder — a one-function change, so the plan does not carry both.

**Sharing.** A sink joins an existing transport for its channel when the
transport's `capture_end` (the latest sink's) is not before the sink's
`capture_start`; otherwise a new transport is opened (and needs a slot,
§3.4). Bytes between two overlapping windows (the 8:30 tail pad and the
9:00 head pad) are written to **both** files. The transport's `cancel`
fires when its last sink ends or is stopped; stopping one sink of two
closes that file and the transport carries on. `holders` (§3.4) reports
sinks per transport so the client never promises a tuner it cannot free.

**Attempts and recovery.** A sink writes to
`<base>.a<N>.part` where `N` is `dvr_recordings.attempt` (1 on first
start). A restart, an owner handoff or a transport failure that leaves the
row `recording` with no live sink is recovered by the owner loop (§3.7
"Recover"): while `now < capture_end − DVR_MIN_USEFUL_S` it opens attempt
`N+1` — a **new file**, so a fenced old process that is still draining can
never write into the new attempt's bytes — and records
`gap_s += now − last_progress`. Finish concatenates the attempt files in
order into `<base>.ts` (MPEG-TS packets concatenate; the discontinuity is
a gap, which the sidecar and `state_reason` name), then removes the
parts. A recording with `gap_s > 0` finishes as `partial`, never `done`;
a recording with 0 bytes on disk at finish is `failed`.

**Drain, shutdown, fence.** Transports are sessions to the manager's
lifecycle paths: `drain_before(generation)` (`live_tv.rs:2510`) cancels
transports with `generation < drain_before` exactly as it cancels sessions;
`shutdown()` (`:2530`) cancels all of them and waits under
`SESSION_DRAIN_TIMEOUT`; and the fan-out checks
`serving.is_current(owner_serving_generation)` every chunk the way
`ensure_session_fence` (`:4022`) does for a session, closing its files
within one `TUNER_READ_TIMEOUT` of authority loss. Rows they leave behind
are `recording` with no worker, which is precisely the state "Recover"
consumes; the new owner's first tick reopens what still has time left.
So the bounded loss on an orderly handoff is one attempt gap of a few
seconds, and on a crash it is the seconds until the process is back —
both proved by M2's tests, not asserted (§10 #1).

**Bytes and progress.** Each sink's byte count is read by the Activity row
and written to `dvr_recordings.bytes` / `last_progress_ms` every 30 s via
`DvrStatePatch::Progress`, which touches **only** those two columns
(§10 #8). `TUNER_READ_TIMEOUT` and the stall mappings (`StreamFailed`,
`:4707` and `:4720–4722`) apply unchanged; a stall closes the transport,
and its sinks are recovered by the next tick like any other worker loss.

**Where the file goes.** `dvr.root` is a container path on a mount every
node sees (`/20t/dvr` in the renders); the owner writes, any node serves.
Basename (§10 #6):

```
 <dvr.root>/<Title>/<Title> - <YYYY-MM-DD HHMM> - <guide_number> - <recording_id[..8]>.ts
   Title, guide_number sanitised: NFKC · strip control chars and / \ : * ? " < > | · collapse whitespace ·
   trim to 120 bytes at a char boundary · never "." or ".." ; the id suffix makes the name unique per row
   attempts:  <base>.a1.part, <base>.a2.part …   opened O_CREAT|O_EXCL — an existing part is never truncated
   sidecar:   <base>.json                        written at finish, and rewritten on delete/restore
```

The scanner (M3) ignores `*.part` and `*.json`, so a growing recording is
never indexed. Sidecar:

```json
{ "plurx_dvr": 1, "recording_id": "…", "channel": {"id": "…", "guide_number": "7.1", "name": "WABC"},
  "airing": {"start": 1789000800, "end": 1789002600}, "capture": {"start": …, "end": …, "actual_end": …},
  "attempts": 2, "gap_s": 41,
  "programme": {"title": "Kitchen Table", "episode_title": "…", "episode": "S03E07", "synopsis": "…",
                "image_url": "…", "original_air_date": "2026-09-13", "series_id": "…", "programme_id": "…"},
  "source": {"video": "mpeg2video", "width": 1920, "height": 1080, "scan": "interlaced", "audio": "ac3", "channels": 6},
  "rule": {"id": "…", "name": "Kitchen Table"}, "state": "partial", "bytes": 2147483648 }
```

### 3.6 Recordings are a library (M3)

`LibraryKind` (`domain.rs:17`) gains `Recordings` (`"recordings"`).
`place_item` (`scan/mod.rs:1163`) gets a fifth arm delegating to a new
`scan/recordings.rs` modelled on `scan/home.rs`: the file becomes a
`Video` item under a `Folder` per programme title (the same shape home
uses, `home.rs:55–95`), and `after_record` reads the sidecar instead of an
NFO and applies a `MetadataPatch` — `title`, `overview` = synopsis,
`air_date` = original_air_date, `recorded_at` = capture start,
`season_number`/`episode_number` parsed from `episode` with the existing
`SXXEYY` regex (`scan/parse.rs:84`), `tags` = `["dvr", "<channel>"]`. No
provider lookups, ever (the `home` rule: `home.rs:1–10`). Artwork from
`image_url` is **not** fetched in this effort: the approved-host list
(`approved_artwork_url`, `comingsoon.rs:534`) does not include
`img.hdhomerun.com` and widening it is its own decision.

The library row is created by the engine the first time `dvr.root` is set
and no `recordings` library has that path: one library named "Recordings",
kind `recordings`, `paths = [dvr.root]`, `scan_interval_mins = 0`. It is
visible under Settings → Libraries like any other, and deleting it there
does not delete files. After a capture finishes, the engine requests a
targeted scan of that library through the existing `scan:library:{id}`
lease path (`state.rs:4138`), and the scan writes back `item_id`/`file_id`
onto `dvr_recordings` by matching the sidecar's `recording_id` — the
recordings shelf (§4.3) joins on that, and playback is the ordinary item
route.

Retention runs in the DVR loop once an hour: for each rule, recordings
beyond `keep` (`last_n` by `airing_start` desc; `until_watched` when the
owner's watch state marks it watched; `days` by age) move to
`state = 'deleted'`, file and sidecar removed, and the next targeted scan
reconciles the item away (`scan_reconcile_items` exists for exactly that).
One-offs are kept until deleted by hand.

### 3.7 The DVR loop (M2) and the reminder loop (M4)

Two tasks on `LiveTvManager`, spawned beside the guide loop in `main.rs`
(`:2223`). Both are owner-only by construction — the guide loop's shape
(`live_tv.rs:3220`: `ours && config.guide_fetches() &&
self.serving.admit().is_some()`) — and take no `job_leases` lease: a lease
would add a second authority to a resource the tuner config already
names.

- **`dvr_loop`**, 15 s tick, runs when `ours && config.enabled &&
  dvr.enabled && serving.admit().is_some()`; a capture must not run with
  the tuner off.
- **`reminder_loop`**, 15 s tick, runs when `ours && serving.admit().is_some()`
  — nothing else. A reminder is a row with a time in it; it needs neither
  the tuner nor the DVR switch (§10 #9). It consults the guide only to
  detect a moved airing, and only when `guide_fetches()`.

`dvr_loop`, each tick, in order — every step reads `config.generation`
once at the top and passes it as the `fence_generation` of every
conditional write, the discipline `start_local_inner` applies with
`registry.min_generation` at `live_tv.rs:2185–2189`:

1. **Recover** — rows in `recording` whose `recording_id` has no live sink
   in the registry (process restart, handoff, transport failure): if
   `now < capture_end − DVR_MIN_USEFUL_S` open attempt `N+1` (§3.5) and
   add `gap_s`; else finish what is on disk as `partial` (or `failed` at
   0 bytes) with reason `worker lost`. Runs first so a restarted owner
   resumes before it schedules anything new.
2. **Expand** — for every enabled rule, match against the owner's guide
   (`GuideCache::read`, `live_tv.rs:1559`), insert-if-absent
   `dvr_recordings` rows (`origin = rule`) for matching airings within
   `guide_hours`; existing rows are never changed here.
3. **Reconcile** — §3.3: re-point `rule_id` on pending rule rows,
   `withdrawn` for rows no enabled rule matches, `stale` for manual rows
   whose programme is gone.
4. **Consume stops** — rows with `stop_requested_at_ms` set and state
   `recording`: cancel the sink (the transport too when it was the last),
   wait for its file to close, finish the row as `done`/`partial` with
   `stopped_by_user_id = stop_requested_by_user_id`. The stop fields are
   written by the route with a conditional update
   (`WHERE state = 'recording' AND stop_requested_at_ms IS NULL`) and read
   here; progress patches never touch them, and a second DELETE is a no-op
   that returns the existing request (§10 #8).
5. **Terminalise** — pending rows with `now ≥ capture_end −
   DVR_MIN_USEFUL_S` become `missed` with reason `capacity|disk|downtime`
   as applicable. Nothing past this point can start (§10 #7).
6. **Allocate** — recompute `scheduled` vs `conflict` per §3.4 over the
   next 14 days; write `state_reason`; also `conflict · disk below floor`
   when the root's free space is under `dvr.free_floor_gb`.
7. **Start** — rows `scheduled` with `capture_start ≤ now < capture_end −
   DVR_MIN_USEFUL_S`: join or open a transport (§3.5), set `recording`,
   `attempt = 1`, `tuner_owner_node_id`, `started_at_ms`, and
   `late_start_s = max(0, now − capture_start)`; a late start is a
   `partial` at finish.
8. **Finish** — sinks whose `capture_end ≤ now`: close, concatenate
   attempts, write the sidecar, `done` (or `partial` when `gap_s > 0 ||
   late_start_s > 0`), request the targeted scan (§3.6). The transport
   closes when its last sink finishes.
9. **Retain** — hourly, §3.6.
10. **Events** — enqueue `recording_started|finished|failed` for the
    webhook worker (§3.8); enqueue, never send, in the loop (§10 #10).

`reminder_loop`, each tick: rows `armed` with `airing_start − lead_s ≤
now` → `fired`, `fired_at_ms`, enqueue the webhook event; rows whose
airing has started → `expired`; when the guide is available, rows whose
`(channel_id, airing_start, title)` no longer exist → `moved`.

`DVR_MIN_USEFUL_S = 60`: an airing with less than a minute of capture left
is not worth a tuner, a file and a library item.

**Owner handoff.** `transition_from_owner_node_id`
(`live_tv.rs:160`) drives `drain_before` on the old owner, which cancels
its transports (§3.5); their rows sit in `recording` with no worker until
the new owner's first tick, whose step 1 reopens them as attempt `N+1`.
The old owner's fenced fan-out cannot write into the new attempt's file.
The loss is the gap between the drain and the first tick on the new
owner, recorded in `gap_s` and visible on the row.

### 3.8 Reminders: three channels, one table (M4–M7)

**Server.** `dvr_reminders` is per user. `GET /dvr/reminders?due=1` returns
the caller's reminders in `fired` state with the programme and whether a
recording covers the airing; `POST /dvr/reminders/{id}/ack` moves it to
`acked` so a second device does not show it again. Firing is the
`reminder_loop` in §3.7 and needs no DVR or tuner state (§10 #9).

**Webhook (R3).** One approved URL (`dvr.webhook_url`), JSON body
`{event: "reminder"|"recording_started"|"recording_finished"|
"recording_failed", ...row}`. Delivery is a **bounded worker, not the
loop** (§10 #10): the loops push onto a `tokio::sync::mpsc` channel of
capacity `DVR_WEBHOOK_QUEUE = 64`; one worker task drains it with a 5 s
timeout and three attempts per event; when the queue is full the oldest
event is dropped and a WARN line names it. Best effort, explicitly: a
missed webhook is a log line, never a queue on disk, and never a delay on
a start or a stop. URL approval is **a new, deliberately weaker policy
than the guide's** (`approved_guide_url`, `guide.rs:76`, is `https` + 443
+ a two-host allowlist): the webhook accepts `https` to any host, or plain
`http` only to an RFC 1918 / link-local address, never with userinfo, and
the redirect policy from `calendar_artwork_client` (`comingsoon.rs:545–559`)
follows nothing off the configured host. Plain HTTP on the LAN is the
point — Home Assistant at `http://ha.lan:8123` is the target — but it is an
outbound-request boundary the server has not had before, so it is ruling 8
in §2, open for Paul; the Developer item's `webhook_url_approved` row says
what the policy accepted.

**In-app overlay (R1).** Every client polls `GET /dvr/reminders?due=1`
every 30 s while the app is foregrounded — web from a single global timer
alongside the 60 s Live TV repaint (`index.html:14946`), Apple from
`LiveTvPlayerController.startGuideRefresh`'s sibling, Android from the
same scope as `LiveTvLease`. The overlay is the render `dvr-tv-reminder`:
lower-left, *Watch* · *Record* · *Dismiss*, a bar counting down to the
start, auto-dismissed at start. **It is buttons, not keys**: it adds no
keydown listener, so `scripts/player-input-fence` (`INDEX_REGIONS`,
`:63–110`; the Live TV region is `:84–89`) needs no new region and the
Live TV adapter (`index.html:15422–15534`) is untouched; on tvOS and
Android TV the three buttons are ordinary focusables and Back/Menu
dismisses through the platform. When the overlay appears during fullscreen
playback it takes focus for its buttons only; the existing player-input
contract stands.

**Phones (R2) — what it can and cannot promise.** A phone mirrors the
server's `armed` reminders into the platform scheduler when the app is
foregrounded: on launch, on foreground, and after every reminder the
phone itself sets or deletes. The mirror is a **reconciliation**, not an
append: the app lists its pending platform notifications (Apple
`getPendingNotificationRequests`, Android its own persisted alarm ids),
cancels those whose reminder is gone or moved on the server, and
schedules the missing ones. What this cannot do, and the docs and the
settings copy say so (§10 #9): a reminder created, deleted or moved from
another device while this phone stays closed is not reflected until the
phone next opens the app — a locally scheduled notification will fire for
a reminder deleted elsewhere, and none will fire for one created
elsewhere. That gap is exactly what R4 (APNs) closes later; R2 is the
honest floor. Apple: `UNUserNotificationCenter` with a
`UNCalendarNotificationTrigger` at `airing_start − lead_s`, category
`plurx.reminder` with actions *Watch* and *Record* (permission asked the
first time a reminder is set, not at launch); Android:
`AlarmManager.setExactAndAllowWhileIdle` into a `BroadcastReceiver` that
posts on a new channel `reminders` (`IMPORTANCE_HIGH`) beside
`offline-downloads` (`OfflineTransferJobService.kt:141–153`), with
`USE_EXACT_ALARM` in the manifest — allowed for a sideloaded build, and
the alternative (`SCHEDULE_EXACT_ALARM`) needs a settings trip on Android
13+. The notification's *Record* action calls `POST /dvr/recordings` for
the airing; *Watch* opens the app on the channel. tvOS has no
user-visible notifications; the overlay is the whole story there.

### 3.9 Attribution: the Activity row (M2, M5)

`Activity` (`http/system.rs:3440`) gains rows of `kind: "record"` from a
new `LiveTvManager::recording_activities()` — label *Recording Kitchen
Table*, detail *7.1 WABC · rule "Kitchen Table (new)" · tuner held on media1 ·
24 min left · 1.9 GB → /20t/dvr/…*, `percent` from elapsed/capture span —
inserted after `live_tv` in `local_activity` (`:3905`, the push at `:3908–3910`) and merged in the
clustered path beside `clustered_live_tv` (`:3629`). The web row
(`renderActivityBody`, `index.html:13970`) gets a Stop that calls
`DELETE /dvr/recordings/{id}` — **the existing `stopSession`
(`:14010`) and `DELETE /activity/sessions/{id}` are transcode/VOD only
(`system.rs:4338–4353`) and are not widened**; Live TV viewer rows keep
having no Stop in this effort (`liveTvActivityRows`, `:13995`).

### 3.10 Settings and the Developer tab (M1, M5)

New keys in `plurx_core::store::keys` (`store/mod.rs:1338`), plain
settings written by `PUT /api/v1/settings` fields of the same name, **not**
riding the Live TV generation CAS (they change no tuner tuple, and
`update_settings` refuses mixing tuner fields with others at
`system.rs:2425`):

| Key | Field | Default | Meaning |
|---|---|---|---|
| `dvr.enabled` | `dvr_enabled` | `0` | the switch |
| `dvr.root` | `dvr_root` | `""` | container path of the DVR root |
| `dvr.free_floor_gb` | `dvr_free_floor_gb` | `50` | below this the scheduler will not start a capture |
| `dvr.tuner_reserve` | `dvr_tuner_reserve` | `1` | slots recordings may never take |
| `dvr.pad_start_s` / `dvr.pad_end_s` | `dvr_pad_start_s` / `dvr_pad_end_s` | `60` / `120` | default padding for new rules and one-offs |
| `dvr.reminder_lead_s` | `dvr_reminder_lead_s` | `300` | default lead for new reminders |
| `dvr.webhook_url` | `dvr_webhook_url` | `""` | R3; empty = off |

The Developer item, built exactly like `library_channels`
(`developer.rs:175–232`), `id: "dvr"`, `setting: Some("dvr_enabled")`:

| Requirement id | Status source |
|---|---|
| `dvr_root_writable` | `statx` + write/remove `.plurx-probe` under `dvr.root` on this node — `Met`/`Unmet`; `Unobservable` on a non-owner node |
| `dvr_free_space` | `statvfs` free ≥ `dvr.free_floor_gb` — `Met`/`Unmet` with both numbers |
| `guide_horizon` | `Met` when `guide_hours > 24` and the cached guide's last `end` is > 24 h out; `Unmet` with "window 4 h — record-what's-on only" otherwise |
| `tuner_reserve` | `dvr.tuner_reserve < max_sessions` — `Met`/`Unmet` |
| `every_node_mounts_root` | `Unobservable` on a single node; from peer capabilities in a cluster |
| `webhook_url_approved` | when `dvr.webhook_url` is set: `Met` when it passes §3.8's policy and a `HEAD` answers within 5 s, `Unmet` with the reason otherwise; absent when empty |

The write path is `put_setting` with a comment matching
`system.rs:3169–3176`: *deliberately no readiness lookup here.*

---

## 4. Contracts

Copied from the tree at `311683bc`; **re-verify each anchor at build
time**.

### 4.1 Guide additions (M0)

```rust
// crates/plurxd/src/live_tv/guide.rs:101 — add to LiveTvProgramme
pub(crate) series_id: Option<String>,     // ≤ 64 bytes, bounded_text
pub(crate) programme_id: Option<String>,  // ≤ 64 bytes
pub(crate) is_new: Option<bool>,          // XMLTV <new/> only; None from HDHomeRun

// guide.rs:349 — HdhrGuideEntry gains (explicit renames: rename_all = "PascalCase" would look for SeriesId)
#[serde(rename = "SeriesID")]
series_id: Option<String>,
#[serde(rename = "ProgramID")]
program_id: Option<String>,

// live_tv.rs:71–73
const DEFAULT_GUIDE_HOURS: u16 = 24;
const MIN_GUIDE_HOURS: u16 = 4;
const MAX_GUIDE_HOURS: u16 = 336;
```

```rust
// guide.rs:48
pub(crate) const GUIDE_MAX_PROGRAMMES_PER_CHANNEL: usize = 800;
// guide.rs:234 — normalise_programmes: overlap resolution moves prev.end, never next.start;
// a new dedupe_programmes(rows) -> rows keyed on (start, title) runs first (§3.2).
```

### 4.2 Schema (M1)

Shared constant `plurx_core::dvr::DVR_SCHEMA` in a new
`crates/plurx-core/src/dvr.rs`, installed by both backends the way
`LIBRARY_CHANNELS_SCHEMA` is (`library_channels.rs:41`; SQLite appends
`MIGRATIONS` v57 at `store/sqlite/mod.rs:1104–1106`; hiqlite adds
`DVR_SCHEMA_VERSION: i64 = 37`, points `AUTH_SCHEMA_VERSION` at it
(`hiqlite.rs:93`), a `MigrateFrom(36)` arm beside `:2362`, and
`install_schema` in the fresh-install list at `:1413–1422` via a new
`hiqlite_dvr.rs` with the `split_schema_statements` splitter,
`hiqlite_library_channels.rs:52`). All clock and random values are
supplied by the application — no `unixepoch()`, no `random()`, no
`AUTOINCREMENT` (the library-channels convention, `library_channels.rs:39`).

```sql
CREATE TABLE IF NOT EXISTS dvr_rules (
    id               TEXT PRIMARY KEY,                       -- uuid v4, app-supplied
    owner_user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    priority         INTEGER NOT NULL,                       -- lower wins; unique per server, renumbered on reorder
    name             TEXT NOT NULL,                          -- what the UI shows, ≤ 80 bytes
    match_mode       TEXT NOT NULL CHECK (match_mode IN ('series_id', 'title')),
    match_value      TEXT NOT NULL,                          -- the id, or the normalised title
    channel_id       TEXT,                                   -- NULL = any channel
    new_only         INTEGER NOT NULL DEFAULT 1,
    keep_mode        TEXT NOT NULL CHECK (keep_mode IN ('all', 'last_n', 'until_watched', 'days')),
    keep_value       INTEGER NOT NULL DEFAULT 0,             -- n, or days; ignored for all/until_watched
    pad_start_s      INTEGER NOT NULL,
    pad_end_s        INTEGER NOT NULL,
    enabled          INTEGER NOT NULL DEFAULT 1,
    created_at_ms    INTEGER NOT NULL,
    updated_at_ms    INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS dvr_rules_priority ON dvr_rules(priority);

CREATE TABLE IF NOT EXISTS dvr_recordings (
    id                       TEXT PRIMARY KEY,
    origin                   TEXT NOT NULL CHECK (origin IN ('manual', 'rule')),
    rule_id                  TEXT REFERENCES dvr_rules(id) ON DELETE SET NULL,   -- owning rule for origin = rule
    requested_by_user_id     INTEGER REFERENCES users(id) ON DELETE SET NULL,
    channel_id               TEXT NOT NULL,
    guide_number             TEXT NOT NULL,
    channel_name             TEXT NOT NULL,
    airing_start             INTEGER NOT NULL,                  -- unix seconds, guide time; the identity
    airing_end               INTEGER NOT NULL,
    capture_start            INTEGER NOT NULL,                  -- airing ± padding
    capture_end              INTEGER NOT NULL,
    title                    TEXT NOT NULL,
    episode_title            TEXT,
    episode                  TEXT,                              -- "S03E07" when known
    synopsis                 TEXT,
    image_url                TEXT,
    original_air_date        TEXT,
    series_id                TEXT,
    programme_id             TEXT,
    state                    TEXT NOT NULL CHECK (state IN
                               ('scheduled','conflict','withdrawn','stale','recording',
                                'done','partial','failed','missed','cancelled','deleted')),
    state_reason             TEXT,                              -- one sentence for every non-scheduled state
    attempt                  INTEGER NOT NULL DEFAULT 0,        -- 0 = never started; N = current .aN.part
    gap_s                    INTEGER NOT NULL DEFAULT 0,        -- seconds not captured between attempts
    late_start_s             INTEGER NOT NULL DEFAULT 0,
    tuner_owner_node_id      TEXT,
    path                     TEXT,                              -- container path of the final .ts (§3.5 basename)
    bytes                    INTEGER NOT NULL DEFAULT 0,
    last_progress_ms         INTEGER,                           -- the only two columns a progress patch writes
    stop_requested_at_ms     INTEGER,                           -- §3.7 step 4; set once, never overwritten
    stop_requested_by_user_id INTEGER,
    item_id                  INTEGER,                           -- filled by the scan (M3)
    file_id                  INTEGER,
    started_at_ms            INTEGER,
    finished_at_ms           INTEGER,
    stopped_by_user_id       INTEGER,                           -- C3 attribution, copied from the request at finish
    created_at_ms            INTEGER NOT NULL,
    updated_at_ms            INTEGER NOT NULL
) STRICT;
-- One airing is one row for life. No partial clause: cancelled and deleted rows keep the
-- identity so expansion cannot resurrect a decision (review finding 2).
CREATE UNIQUE INDEX IF NOT EXISTS dvr_recordings_airing ON dvr_recordings(channel_id, airing_start);
CREATE INDEX IF NOT EXISTS dvr_recordings_state ON dvr_recordings(state, capture_start);

CREATE TABLE IF NOT EXISTS dvr_reminders (
    id             TEXT PRIMARY KEY,
    user_id        INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel_id     TEXT NOT NULL,
    guide_number   TEXT NOT NULL,
    airing_start   INTEGER NOT NULL,
    airing_end     INTEGER NOT NULL,
    title          TEXT NOT NULL,
    lead_s         INTEGER NOT NULL,
    state          TEXT NOT NULL CHECK (state IN ('armed', 'fired', 'acked', 'expired', 'moved')),
    fired_at_ms    INTEGER,
    acked_at_ms    INTEGER,
    created_at_ms  INTEGER NOT NULL,
    updated_at_ms  INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS dvr_reminders_user_airing ON dvr_reminders(user_id, channel_id, airing_start);
```

Bounds, as named consts in `plurx_core::dvr` (the library-channels
habit, `library_channels.rs:13–37`): `DVR_RULES_MAX = 200`,
`DVR_RULE_NAME_MAX = 80`, `DVR_MATCH_VALUE_MAX = 200`,
`DVR_RECORDINGS_LIST_PAGE = 100`, `DVR_REMINDERS_PER_USER_MAX = 200`,
`DVR_SCHEDULE_DAYS_MAX = 14`, `DVR_PAD_MAX_S = 3600`, `DVR_LEAD_MAX_S =
3600`, `DVR_MIN_USEFUL_S = 60`, `DVR_WEBHOOK_QUEUE = 64`,
`DVR_ATTEMPTS_MAX = 8`.

**Store trait** `DvrStore` in `store/mod.rs`, added to both the `Store`
supertrait list (`:4376–4383`) and the blanket impl (`:4408–4415`):

```rust
#[async_trait]
pub trait DvrStore: Send + Sync + 'static {
    async fn list_dvr_rules(&self) -> Result<Vec<DvrRule>, StoreError>;
    async fn put_dvr_rule(&self, rule: &DvrRule) -> Result<(), StoreError>;          // upsert by id
    async fn delete_dvr_rule(&self, id: &str) -> Result<bool, StoreError>;
    async fn reorder_dvr_rules(&self, ids_in_priority_order: &[String]) -> Result<(), StoreError>;
    async fn insert_dvr_airing_if_absent(&self, row: &DvrRecording) -> Result<InsertOutcome, StoreError>;
        // Inserted | Exists(existing_state) — never updates; the unique index decides
    async fn list_dvr_recordings(&self, filter: DvrRecordingFilter, after_id: Option<&str>, limit: i64)
        -> Result<Vec<DvrRecording>, StoreError>;
    async fn list_dvr_recordings_in(&self, states: &[DvrState]) -> Result<Vec<DvrRecording>, StoreError>;
    /// Conditional: applies only when the row is in one of `from` and, for owner writes,
    /// `fence_generation` is still the current tuner generation. Returns false otherwise.
    async fn transition_dvr_recording(&self, id: &str, from: &[DvrState], to: DvrState, reason: Option<&str>,
        patch: DvrStatePatch, fence_generation: Option<i64>) -> Result<bool, StoreError>;
    /// Writes only bytes and last_progress_ms; never touches state or the stop fields.
    async fn progress_dvr_recording(&self, id: &str, bytes: i64, at_ms: i64) -> Result<(), StoreError>;
    /// UPDATE … SET stop_requested_at_ms = $2, stop_requested_by_user_id = $3
    /// WHERE id = $1 AND state = 'recording' AND stop_requested_at_ms IS NULL — returns the row after.
    async fn request_dvr_stop(&self, id: &str, at_ms: i64, by_user: i64) -> Result<DvrRecording, StoreError>;
    async fn repoint_dvr_rule_rows(&self, changes: &[(String, Option<String>)]) -> Result<(), StoreError>;
    async fn link_dvr_recording_media(&self, recording_id: &str, item_id: i64, file_id: i64) -> Result<bool, StoreError>;
    async fn list_dvr_reminders(&self, user_id: i64, state: Option<ReminderState>) -> Result<Vec<DvrReminder>, StoreError>;
    async fn put_dvr_reminder(&self, r: &DvrReminder) -> Result<(), StoreError>;
    async fn delete_dvr_reminder(&self, user_id: i64, id: &str) -> Result<bool, StoreError>;
    async fn transition_dvr_reminders(&self, now_ms: i64, fire_before: i64) -> Result<Vec<DvrReminder>, StoreError>;
}
```

Every hiqlite statement obeys the `$N` first-appearance rule
(`validate_parameter_order`, `hiqlite.rs:3992–4076`; the `?`/`:` refusal is `:4066–4070`): `$1, $2, …` in
order of first use, re-use allowed, no forward jumps, no `?`/`:`. The
SQLite twin uses `?N`. The three-voter Store contract
(`make cluster-store-check`) gains one scenario per method group.

### 4.3 Routes (M1 skeleton, M2/M4 behaviour)

All under `/api/v1/dvr`, registered in `http/mod.rs` beside the Live TV
block (`:99–132`) as `.nest("/dvr", dvr::router())`, each with a row in
`docs/API.md` in a new "DVR" block after the Live TV block (`:1822–1833`)
and the route count at `docs/API.md:17–18` updated —
`tests/operations/test_api_doc_routes.py` fails otherwise.

| Method | Path | Credential | Does |
|---|---|---|---|
| GET | `/dvr/status` | bearer | `{enabled, owner_node_id, root, free_bytes, floor_bytes, slots: {max, reserve, recording}, next_start}` |
| GET | `/dvr/recordings?state=&after=&limit=` | bearer | rows; `state` filters; default excludes `deleted` |
| POST | `/dvr/recordings` | bearer | one-off: `{channel_id, airing_start}` → server copies the programme from the guide; or manual `{channel_id, capture_start, capture_end, title}` (no guide needed — this is the free-tier story). `409 airing_unknown` when neither resolves |
| GET | `/dvr/recordings/{id}` | bearer | one row with `item_id`/`file_id` when linked |
| DELETE | `/dvr/recordings/{id}` | bearer | `scheduled`/`conflict`/`withdrawn`/`stale` → `cancelled` (durable; expansion never brings it back); `recording` → records the stop request (§3.7 step 4) and answers `202 {pending: true, requested_at}` — a repeat answers the same; `done`/`partial`/`failed`/`missed` → `deleted` and the files removed (requires `?delete_file=1`, else `409`) |
| POST | `/dvr/recordings/{id}/restore` | bearer | `cancelled` → `scheduled` when `now < capture_end − 60 s`, else `409 airing_past`; the only way back from a cancel |
| GET | `/dvr/schedule?days=14` | bearer | `dvr_recordings` in `scheduled`/`conflict`/`withdrawn`/`stale`/`recording` ordered by `capture_start`, plus `conflicts: N`; `cancelled` rows included only with `&cancelled=1` (so *Skip* stays visible and reversible) |
| GET | `/dvr/rules` | bearer | priority order |
| POST | `/dvr/rules` | bearer | create; `from_airing: {channel_id, airing_start}` fills mode/value/channel from the guide |
| PUT | `/dvr/rules/{id}` | bearer (owner or admin) | edit |
| DELETE | `/dvr/rules/{id}` | bearer (owner or admin) | delete; its pending rows become `withdrawn` on the next tick (§3.3) — not `cancelled`, because that is the user's word |
| PUT | `/dvr/rules/order` | admin | `{ids: [...]}` renumbers `priority` |
| GET | `/dvr/reminders?due=1` | bearer | caller's reminders; `due=1` = `fired` only, each with `covered_by_recording: bool` |
| POST | `/dvr/reminders` | bearer | `{channel_id, airing_start, lead_s?}` |
| DELETE | `/dvr/reminders/{id}` | bearer | own only |
| POST | `/dvr/reminders/{id}/ack` | bearer | `fired` → `acked` |

Typed errors use the Live TV envelope (`LiveTvFailure` on the clients,
`LiveTv.swift:443`, `LiveTvApi.kt:300`) with new codes `dvr_disabled`,
`airing_unknown`, `airing_past`, `rule_limit`, `reminder_limit`, `not_owner`. The
`tuner_capacity` body gains the optional `holders` array (§3.4); clients
that do not know the field ignore it.

Routes mutate replicated rows only; the owner loop is the sole writer of
`recording`/`done`/`partial`/`failed`/`missed`. A DELETE on a `recording`
row — from any node, the owner included — is `request_dvr_stop` (§4.2): a
conditional write of the two stop columns that succeeds once and is then
idempotent, answered `202 {pending: true}`; the owner's next tick consumes
it (§3.7 step 4) and the row leaves `recording` only when the file is
closed and the slot released. The UI shows "stopping…" and polls the row;
a Watch retry after a capacity dialog waits for that transition (§3.4).
Same shape as `DELETE /live-tv/sessions/{cap}` crossing to the owner
(`capability_owner` → `owner_stop`, `http/live_tv.rs:460`), without a new
internal route and without a field that progress writes could clobber
(§10 #8).

### 4.4 Registry and engine (M2)

```rust
// live_tv.rs:1182 — LiveTvRegistry gains
transports: HashMap<String, Arc<DvrTransport>>,   // key = channel id; one tuner GET each

pub(crate) struct DvrTransport {
    pub(crate) channel: LiveTvChannel,
    pub(crate) generation: i64,                 // config generation at open — drain_before compares this
    pub(crate) owner_serving_generation: u64,   // serving.is_current() checked per chunk
    pub(crate) cancel: CancellationToken,       // fires when the last sink ends, on drain, on shutdown
    pub(crate) delivered: Arc<AtomicU64>,
    pub(crate) sinks: StdMutex<Vec<Arc<DvrSink>>>,
    pub(crate) worker: StdMutex<Option<JoinHandle<Result<(), LiveTvError>>>>,   // cancel_and_wait shape
}

pub(crate) struct DvrSink {
    pub(crate) recording_id: String,
    pub(crate) attempt: u32,
    pub(crate) window: (i64, i64),              // [capture_start, capture_end)
    pub(crate) file: StdMutex<Option<std::fs::File>>,   // O_CREAT|O_EXCL on <base>.a<N>.part
    pub(crate) bytes: AtomicU64,
    pub(crate) cancel: CancellationToken,       // stop one sink; the transport lives while any sink does
}

impl LiveTvRegistry {
    fn held(&self) -> usize;                                       // sessions + transports
    fn may_open_transport(&self, max_sessions: u8, reserve: u8) -> bool;   // §3.4, both conditions
}

impl LiveTvManager {
    pub(crate) async fn dvr_loop(self: Arc<Self>, shutdown: CancellationToken);       // §3.7
    pub(crate) async fn reminder_loop(self: Arc<Self>, shutdown: CancellationToken);  // §3.7
    async fn dvr_tick(&self, config: &LiveTvConfig, dvr: &DvrConfig) -> Result<(), LiveTvError>;
    async fn attach_sink(&self, config: &LiveTvConfig, row: &DvrRecording, attempt: u32)
        -> Result<(), LiveTvError>;                                 // joins or opens a transport
    async fn stop_sink(&self, recording_id: &str) -> Result<bool, LiveTvError>;       // consumed stop
    pub(crate) fn recording_activities(&self) -> Vec<DvrActivity>;  // §3.9
    pub(crate) fn transport_holders(&self) -> Vec<DvrHolder>;       // for the tuner_capacity body
}

async fn pump_tuner_fanout(input: LiveTunerInput, transport: Arc<DvrTransport>, cancel: CancellationToken)
    -> Result<(), LiveTvError>;   // mirrors pump_tuner_stream (:4693): one task owns the response

// drain_before (:2510), shutdown (:2530) and cancel_and_wait extend to transports;
// the fan-out checks serving.is_current(owner_serving_generation) per chunk (cf. ensure_session_fence :4022).
```

`DvrConfig::from_snapshot(settings)` follows `LiveTvConfig::from_snapshot`
(`live_tv.rs:203`) and the `stored_switch` parser
(`plurx-core/src/store/fragment_index_cluster.rs:653` — not the same-named
daemon file). The viewer admission text at `live_tv.rs:2200–2205` becomes
`registry.held() >= max_sessions`.

### 4.5 Client contract (M5–M7)

What every client renders from the same rows:

- **Cell marks:** a row in `/dvr/schedule` for `(channel_id, airing_start)`
  → dot (`scheduled`), two dots when `rule_id` is set, `REC` + progress
  underline when `recording`, an amber dot when `conflict`, a hollow dot
  when `withdrawn`/`stale` (the Scheduled list says why), nothing for
  `cancelled` unless the list is showing cancelled rows; a row in
  `/dvr/reminders` → bell. Fetched once per guide load and on every
  mutation; not polled.
- **Cell actions:** Watch (existing tune / "Watch at hh:mm" reminder
  shortcut when in the future) · Record (`POST /dvr/recordings`) · Record
  series (`POST /dvr/rules {from_airing}` then open the rule editor with
  the created rule) · Remind me (`POST /dvr/reminders`). The tuner line
  under the buttons is computed client-side from `/dvr/status.slots` and
  the schedule.
- **Recordings segment:** third segment in the Live TV toolbar on all
  three (web `.lt-switch`, `index.html:15131`; Apple `liveToolbar`,
  `LiveTvView.swift:1490`; Android `LiveTvPhoneToolbar`, `LiveTvScreen.kt:789`, and the segment
  row inside `TelevisionLiveTvBrowser`, `:922`/`:1003`), chips Library · Scheduled ·
  Rules inside it. Library rows link to the item's normal detail/play;
  a `recording` row shows Stop.
- **Overlay:** §3.8; shown on any page while `due` is non-empty and not
  acked; *Dismiss* acks.
- **Developer tab:** web only (the native settings surfaces do not carry
  Developer), the item rendered by `devReq` rows (`index.html:15839`)
  under a new `setCard` with `togRow("dvrenabled", …)` (`:14398`, model
  at `:15907`).

---

## 5. Guardrails — do not

1. **Do not gate.** No enable path reads readiness; `dvr.enabled` is the
   only switch and it is a plain setting. The Developer rows are advisory
   (`developer.rs:11–16` is the law here).
2. **Do not open a second tuner GET for a viewer or a capture.** One GET
   per capture, rotated files for back-to-back airings (§3.5). The test at
   `live_tv.rs:6113` stays green and gains a capture twin.
3. **Do not put the guide in the replicated store.** #273's ruling: owner-
   local `guide.json`. The DVR reads the owner's in-memory guide; only
   materialised airings are replicated.
4. **Do not widen `DELETE /activity/sessions/{id}` to Live TV or DVR.**
   Recordings stop through `DELETE /dvr/recordings/{id}`; viewer rows keep
   no Stop in this effort.
5. **Do not add a keydown listener for the overlay.** Buttons only; the
   `player-input-fence` region list is unchanged.
6. **Do not fetch artwork from `img.hdhomerun.com`.** The approved-host
   list is an SSRF boundary (`comingsoon.rs:516–522`); widening it is a
   separate decision.
7. **Do not touch the reducer, the lease, the input contract, the theme
   tokens, or the proportions constants** (`LiveTvType`, `LiveTvGridMetrics`
   on either native client). The cell marks fit inside the existing 74 px /
   44 px rows.
8. **Do not pre-empt.** A recording never takes a viewer's tuner; a viewer
   never takes a recording's without the confirm and the attribution — and
   never a shared transport's on the strength of stopping one of its sinks.
11. **Do not update on conflict.** Rule expansion is insert-if-absent;
    `cancelled` is a durable user decision reversed only by `restore`.
12. **Do not send from the loop.** Webhooks go through the bounded worker;
    a tick never awaits the network.
9. **Do not use `unixepoch()`, `random()`, `AUTOINCREMENT` or `?`
   placeholders in replicated SQL.** `validate_sql` refuses the last; the
   first three break determinism across voters.
10. **Do not create the recordings library silently more than once.** One
    library per root; if an admin deleted it, the engine recreates it and
    logs at INFO.

---

## 6. Milestones

Each milestone: one task PR into `effort/live-tv-dvr`, draft → one
adversarial review → ready, which runs the lane → merge. The lane promotes to
`main` once after M4 (server complete) and once after M7 (clients), each
time with the one full qualification run. Apple `CURRENT_PROJECT_VERSION`
(`project.yml:18`, now 146) and Android `versionCode`
(`build.gradle.kts:48`, now 89) bump in the PR that first touches each
client's sources — `validation/mobile_versions.py:213–224` refuses the
merge otherwise.

**Ordering against the reliability effort (§10 #11).** The reliability
plan's own milestones put guide durability in its **M1**, client guide
polling in **M2** and same-viewer stray eviction in **M3**. Two pins,
by commit on `main`, never by branch:

| DVR milestone | Needs on `main` first | Why |
|---|---|---|
| M0 | reliability M1 (`guide.json` persistence) | the incremental 14-day extension accumulates in that cache |
| M2 promote | reliability M3 (stray eviction before `tuner_capacity`) | the `holders` dialog assumes a viewer's own stray was already evicted; without it the dialog blames a recording for a slot the viewer holds |

Every DVR PR is based on `main` (rebased onto it when it moves). If the
reliability commit a milestone needs is not on `main` yet, that milestone
waits; it is never based on `effort/live-tv-reliability`.

### M0 — Guide identity and horizon (server)

**Base:** `main` at or after the reliability M1 merge. **Files:** `live_tv/guide.rs` (fields, renames, caps,
`normalise_programmes`, `dedupe_programmes`), `live_tv.rs` (constants,
`validate_guide`, extension loop), `http/live_tv.rs:32`, `http/system.rs`
(`u16`), `docs/API.md:1825`, `LiveTvGuide.swift`, `LiveTvApi.kt` models
(optional fields only, no UI).

Build §4.1; make the per-channel extension incremental against the
persisted cache (§3.2); raise the settings bound and both caps. Unit
tests: an HDHomeRun fixture entry with `SeriesID`/`ProgramID` round-trips
(and a fixture without them parses as before); an XMLTV `dd_progid` yields
both ids; a 336-hour window with a fixture that answers 4 h per page fills
across ticks within the 64-request budget, holds more than 200 rows on a
channel, and stops asking a channel that repeats; an overlap in the source
shortens the earlier row and never moves a `start`; merging a cached page
with a fresher one that carries ids yields one row with the ids.

**Acceptance:** `cargo test -p plurxd live_tv::guide` green;
`tests/playback/live-tv-guide-cases.json` gains one case with ids and both
native clients decode it (`make apple-test` on `maca`, Android unit test);
`GET /live-tv/guide?hours=336` on a fixture owner returns programmes past
72 h and more than 200 per channel; `python3 -m unittest
tests.operations.test_api_doc_routes` green after the `:1825` row edit.

### M1 — Store, settings, routes, Developer item (server, no engine)

**Files:** new `crates/plurx-core/src/dvr.rs`, `store/mod.rs` (trait,
keys), `store/sqlite/dvr.rs`, `store/hiqlite_dvr.rs`, `hiqlite.rs`
(version 37), `http/dvr.rs`, `http/mod.rs`, `http/developer.rs`,
`http/system.rs` (settings fields), `docs/API.md`.

Build §4.2, §4.3 with every route answering from the Store (no owner
loop yet: `POST /dvr/recordings` writes `scheduled`; nothing starts). The
Developer item per §3.10. Store tests, on both backends: the review's
finding-2 reproduction as a contract — insert a row, cancel it, call
`insert_dvr_airing_if_absent` for the same `(channel_id, airing_start)`
→ `Exists(cancelled)` and still one row; `request_dvr_stop` succeeds once
and is idempotent while `progress_dvr_recording` runs concurrently;
`transition_dvr_recording` refuses a stale `fence_generation`.

**Acceptance:** `cargo test -p plurx-core dvr` and `-p plurxd http::dvr`
green; `make cluster-store-check` includes the DVR scenarios and passes in
the cloud container (~21 min); `python3 -m unittest
tests.operations.test_api_doc_routes` green with the new count;
`developer_readiness_reports_what_it_reads_and_admits_what_it_cannot`
(`http/mod.rs:4937`) extended to assert the `dvr` item and that
`PUT /settings {dvr_enabled: true}` succeeds with every requirement
`Unmet`.

### M2 — Recording engine (server)

**Files:** `live_tv.rs` (registry, loop, capture, activities),
`main.rs` (spawn), `http/system.rs` (Activity rows), `http/dvr.rs`
(DELETE on `recording`), `http/live_tv.rs` (`holders` on capacity).

Build §3.3 reconciliation, §3.4, §3.5, §3.7 (`dvr_loop`, all ten steps),
§3.9. Tests, each against the fake-tuner/fake-clock fixtures the live
engine already has (`live_tv.rs:6113` neighbourhood); the ones marked ★
are the review's required regression cases (§10):

- a scheduled airing starts at `capture_start` and the fixture sees exactly
  one GET; ★ a scheduled airing whose `capture_end` has passed (restart
  after the airing; capacity freed after the airing) becomes `missed` and
  opens **no** GET; ★ a restart *during* the airing reopens attempt 2 with
  `gap_s > 0` and finishes `partial`, and the abandoned `.a1.part` is
  concatenated, not truncated;
- ★ admission in both orders: three transports then a viewer → admitted;
  one viewer then two transports then a third → admitted; a fourth
  transport → `conflict` with the reserve reason; four viewers then a
  transport → refused; a full tuner set refuses everyone;
- ★ back-to-back airings on one channel produce two files from one GET
  with the overlap bytes in both; stopping either sink during the overlap
  leaves the other recording running and the transport open; stopping the
  last sink closes the transport and frees the slot; `holders` reports two
  sinks and the client contract's "no single-stop offer" condition;
- ★ cancel an airing, run expansion five times → still exactly one row,
  `cancelled`; `restore` → `scheduled`; cancel again → `cancelled`;
- ★ reconciliation: disable a rule → its pending rows `withdrawn`, its
  `recording` row untouched; re-enable → re-materialised; raise another
  matching rule's priority → `rule_id` re-pointed; a manual row survives
  every rule edit; a moved programme → rule row `withdrawn`, manual row
  `stale`, the new airing materialised at the new start;
- ★ stop from a non-owner node while a progress patch is being persisted
  every 30 s: the stop fields survive, a second DELETE returns the same
  request, the owner consumes it once, `stopped_by_user_id` is the
  requester; the Watch retry after the stop succeeds only after the row
  has left `recording`;
- ★ two recordings with identical titles and start times on different
  channels get distinct paths and both finish;
- ★ drain: `drain_before(gen+1)` cancels transports opened at `gen`;
  `shutdown()` cancels all and waits; a fenced transport (serving
  generation advanced) closes its files within one read timeout; the rows
  are then recovered by the next tick;
- a config generation bump mid-tick lands no row from the old generation;
  free space below the floor yields `conflict · disk` and starts nothing.

**Acceptance:** those tests green; on the lab box with the FLEX 4K,
record one 5-minute airing **and** a pair of overlapping airings on one
channel, then `POST /api/v1/decision` and a session on each resulting
`.ts` — including the second file of the shared transport — play on the
lab Mac's browser (the raw-TS proof, §3.5); with one recording running,
three viewers on other channels all start; the Activity page shows the
row with bytes advancing and Stop works from a non-owner node.

### M3 — Recordings library kind and scan (core)

**Files:** `domain.rs`, `scan/mod.rs`, new `scan/recordings.rs`,
`scan/parse.rs` (no change expected), `store/mod.rs` (`link_dvr_recording_media`
caller), settings UI for library kind on web (one `<option>`).

Build §3.6. Tests: a sidecar + `.ts` under a `recordings` root scans into
Folder/Video with the patch applied and `item_id`/`file_id` written back
to the recording; a `.ts.part` is skipped; deleting a recording removes
the file and the next scan reconciles the item; `until_watched` retention
respects the owner's watch state.

**Acceptance:** `cargo test -p plurx-core scan::recordings` green; the
M2 lab recording appears on the Recordings shelf with title, synopsis and
air date from the sidecar and plays from the shelf.

### M4 — Reminders and the webhook (server)

**Files:** `live_tv.rs` (tick step 5), `http/dvr.rs` (reminders),
`live_tv/webhook.rs` (new; approval + client), `docs/API.md`.

Build §3.7 `reminder_loop`, §3.8 server half, the webhook worker. Tests
(★ = the review's required cases, §10): a reminder fires at `airing_start
− lead_s` and not before; ★ it fires with `dvr.enabled = 0` and with
`live_tv.enabled = 0`; a second ack is idempotent; a guide refresh that
renames the airing marks it `moved`; the webhook client refuses a non-
approved host and follows no off-host redirect; ★ six due reminders
against an endpoint that never answers, with a capture due to start and
another due to stop in the same tick: both transitions happen on time
and the worker's queue drains (or drops, with the WARN line) on its own.

**Acceptance:** tests green; on the lab box, a reminder set 6 minutes
ahead posts to a local `nc -l` (or Home Assistant) within 15 s of the lead
time.

Promote `effort/live-tv-dvr` → `main` here (server complete, clients
build against a merged API).

### M5 — Web

**Files:** `crates/plurxd/src/web/index.html`, `web/live-tv.js` (pure
schedule/marks helpers), `tests/ui-structure.golden` (regenerated),
`tests/web/*` for the pure helpers.

Build §4.5 for the web: popover actions replacing the "Live only" hint
(`index.html:15322–15336`), cell marks in `liveTvGridMarkup`
(`:15226`) and the list rows, the Recordings segment, the overlay, the
Activity Stop, the Developer card. Follow the renders `dvr-web-*.png`.

**Acceptance:** `make web-check`; `scripts/ui-baseline --self-host
--check` green with the regenerated golden; `scripts/player-input-fence`
green with no region change; a 1440-wide screenshot of each of guide
popover, Recordings, Activity and Developer attached to the PR.

### M6 — Apple

**Files:** `clients/apple/Sources/LiveTvView.swift` (actions in
`focusedProgrammeDetails` `:1951` and `programmeDetail` `:2453`,
Recordings segment in `liveToolbar` `:1490`, marks in
`LiveTvGuideGrid`/`LiveTvChannelRow`), new `Sources/DvrClient.swift`,
`Sources/ReminderOverlay.swift`, `Sources/LocalReminders.swift`
(UNUserNotificationCenter, iOS only), `project.yml` (build bump; no new
entitlements — local notifications need none).

Follow `dvr-tv-guide.png`, `dvr-tv-reminder.png`, `dvr-tv-recordings.png`,
`dvr-phone-*.png`. Type sizes and grid metrics unchanged (guardrail 7).

**Acceptance:** `make apple-test` on `maca` green; a 1080p tvOS simulator
screenshot of the guide with a focused future cell showing the four
actions and of the overlay over playback; an iPhone screenshot of the
programme sheet; a local notification observed firing on a simulator with
a 1-minute lead.

### M7 — Android

**Files:** `livetv/LiveTvScreen.kt` (`LiveTvFocusedProgramme` `:1543`,
`LiveTvProgrammeDetail` `:1902`, toolbars `:789`/`:1003`),
`livetv/LiveTvGuideUi.kt` (marks), new `livetv/DvrApi.kt`,
`livetv/ReminderOverlay.kt`, `reminders/ReminderAlarmReceiver.kt`,
`AndroidManifest.xml` (`USE_EXACT_ALARM`, receiver), `build.gradle.kts`
(versionCode bump).

**Acceptance:** Android unit tests green; emulator screenshots matching
M6's set; an exact alarm observed posting the `reminders` notification
with *Watch* and *Record* actions.

Promote `effort/live-tv-dvr` → `main` (clients).

### M8 — Hardware pass and status

A GPT session runs §8's two prompts on the physical FLEX 4K, Apple TV and
phones; findings become a `LIVE-TV-DVR-STATUS.md` in the shape of
`LIVE-TV-NATIVE-LAYOUTS-STATUS.md` (`:1–13` header + companion block) with
an evidence table; `STATUS.md` gets its section in the same commit.

---

## 7. Parallel handoffs — file ownership

If M5–M7 run as three sessions at once, each owns only its tree and
nothing else:

| Session | Owns | Never touches |
|---|---|---|
| web | `crates/plurxd/src/web/**`, `tests/ui-structure.golden`, `tests/web/**` | `crates/plurxd/src/*.rs`, clients |
| Apple | `clients/apple/**` | everything else |
| Android | `clients/android/**` | everything else |

All three read §4.3/§4.5 as the contract and build against the merged
server. A client that needs a server change stops and files it against
the server owner rather than editing `http/dvr.rs`. `STATUS.md` is edited
only by the promoting PR to avoid the top-of-file conflict every PR here
would otherwise hit.

---

## 8. Hardware prompts (GPT)

**Guide capability (before M0 is merged):**

> On the FLEX 4K (device `1040A1B2`, LAN), read `http://<tuner-ip>/discover.json`
> and take `DeviceAuth`. Fetch with curl and `--compressed`, twice:
> `https://api.hdhomerun.com/api/guide?DeviceAuth=<auth>` and the same with
> `&Channel=7.1&Start=<unix time 48 hours from now>`. For each report HTTP
> status, gzip yes/no, channel count, and for channel 7.1 the earliest and
> latest `StartTime`/`EndTime` as local times; paste one entry verbatim
> with `ImageURL` shortened, and say whether it has `SeriesID`,
> `ProgramID` and `Filter`. If the second call returns nothing past ~4 h,
> say so — that is the free tier, and the plan's M0 still stands. Replace
> `DeviceAuth` with `<auth>` in the report.

**End-to-end (M8):**

> With `dvr.enabled` on and `dvr.root` set to a mount every node sees:
> (1) from the Apple TV guide, Record a programme starting within 10
> minutes on an unprotected ATSC 1.0 channel; confirm the Activity page
> shows a "record" row with a rising byte count and that `ps` on the owner
> shows no ffmpeg for it; (2) while it records, start three live viewers
> on other channels from three clients — all three should play (one
> recording + three viewers fits four tuners); then start a fourth viewer
> and report the exact message and whether it offers to stop the
> recording; (3) let
> it finish; confirm the file and `.json` sidecar under the root, that it
> appears on Recordings on all three clients within two minutes, and that
> it plays on the Apple TV and in Chrome; (4) set a reminder 6 minutes
> ahead on the iPhone, lock the phone, and report the notification's text
> and whether *Record* from it creates the recording; (5) with the app
> open on the Apple TV, report the overlay's appearance, focus behaviour
> and auto-dismiss. Include screenshots and the owner's INFO log lines
> for the recording's start and finish.

---

## 9. What this plan leaves for later, on purpose

Play-while-recording and a time-shift buffer (the `.part` file is the
seed; the VOD path must first accept a growing input); "file into Shows"
(L3); APNs (R4 — a `.p8` in settings, `tokens.device` rows,
`hiqlite.rs:199`); artwork from the guide's image host; commercial
detection; recording protected ATSC 3.0 (the engine cannot open it).

---

## 10. Review dispositions — Astra, 2026-09-13

Eleven findings, all accepted; none rejected. Each row names where the
change now lives. "Test" is the regression case the review required,
now listed under the milestone that owns it.

| # | Finding | Disposition | Where |
|---|---|---|---|
| 1 (P1) | Owner handoff and crashes cannot resume; DB fencing does not stop an old process writing the shared file; captures ignore drain/shutdown/authority | **Accepted.** Attempts as separate `.a<N>.part` files (a fenced old process can never write into the new attempt), `Recover` as step 1 of every tick, `gap_s`/`late_start_s` on the row, `partial` whenever a gap exists; transports participate in `drain_before`, `shutdown`, `cancel_and_wait` and the per-chunk serving-authority check | §3.5 "Attempts and recovery", "Drain, shutdown, fence"; §3.7 step 1, "Owner handoff"; §4.2 columns; §4.4; M2 tests ★ |
| 2 (P1) | Partial unique index lets a cancelled airing return on the next tick | **Accepted.** Unique index over all states; expansion is insert-if-absent; `cancelled` is durable; `POST …/restore` is the explicit way back | §3.3; §4.2 index + `insert_dvr_airing_if_absent`; §4.3; guardrail 11; M2 test ★ |
| 3 (P1) | `free_slots ≥ 1 + reserve` makes admission depend on start order | **Accepted.** Two occupancy conditions under one lock: `held() < max_sessions` and `transports < max_sessions − reserve`; M2 and M8 acceptance corrected (three recordings *allow* a viewer; one recording + three viewers fits) | §3.4; §4.4 `may_open_transport`; M2 tests ★; §8 prompt |
| 4 (P1) | Registry keyed by recording id cannot express a shared GET; stop dialog over-promises | **Accepted.** `DvrTransport` per channel with `DvrSink`s; transports count, sinks do not; the last sink closes the transport; `holders` reports sinks and the single-stop offer is made only for one-sink transports | §3.4; §3.5 "Sharing"; §4.4; M2 tests ★ |
| 5 (P1) | No reconciliation for disabled/edited rules, moved programmes, multiple matches; tail-only extension never revalidates | **Accepted.** `origin`/`rule_id` ownership, `withdrawn` and `stale` states with the rules in §3.3, reconciliation as step 3; a revalidation day cycles through the cached horizon each tick | §3.3; §3.7 step 3; §3.2 (revalidation sentence); §4.2; M2 tests ★ |
| 6 (P1) | Filename not unique per recording; recovery may truncate a part | **Accepted.** `guide_number` and `recording_id[..8]` in the basename, sanitisation rules, `O_EXCL` creation, attempts never reuse a name | §3.5 "Where the file goes"; M2 test ★ |
| 7 (P1) | Expired scheduled rows record whatever is on air, then finish `done` | **Accepted.** `now < capture_end − DVR_MIN_USEFUL_S` on every start and recover; `missed` state written by a terminalise step that runs before allocation; late starts are `partial` | §3.7 steps 5 and 7; §4.2 `missed`; M2 tests ★ |
| 8 (P1) | Stop is an unstructured `state_reason` note the tick never consumes; progress writes can clobber it | **Accepted.** `stop_requested_at_ms` / `stop_requested_by_user_id` written by a conditional update that succeeds once; `progress_dvr_recording` writes only `bytes`/`last_progress_ms`; step 4 consumes; DELETE answers `202 {pending}`; Watch retries only after the row leaves `recording` | §3.7 step 4; §4.2 columns + trait; §4.3; M2 test ★ |
| 9 (P2) | Reminders inherit the tuner/DVR prerequisites; foreground-only phone mirroring is oversold | **Accepted.** Separate `reminder_loop` gated on owner + serving only; phone mirror defined as a reconciliation of pending platform notifications with the limitation stated in the docs and settings copy; R4 named as what closes it | §3.7; §3.8 "Phones"; M4 test ★ |
| 10 (P2) | Three 5 s webhook attempts inside the loop stall recording control | **Accepted.** Bounded mpsc (64) + one worker task, drop-oldest with a WARN; loops enqueue only | §3.8 "Webhook"; §3.7 step 10; guardrail 12; M4 test ★ |
| 11 (P2) | "After reliability M0–M2" does not deliver the admission behaviour §3.4 assumes | **Accepted.** Pinned by commit on `main`: reliability M1 before DVR M0, reliability M3 before DVR M2 promotes; no DVR PR based on the reliability effort branch | §2; §6 "Ordering against the reliability effort" |
| — | Raw TS capture is a reasonable start; keep the playback acceptance including the shared transport's second file | **Agreed.** M2 acceptance plays both files through the real path; the `-c copy` fallback stays a one-function change | §3.5; M2 acceptance |

Two things the review did not ask for but the fixes made necessary: the
`stale` state for manual rows whose programme moved (finding 5 defines
withdrawal for rule rows; a manual row is the user's, so it is surfaced,
not withdrawn), and `DVR_ATTEMPTS_MAX = 8` so a transport that fails
every tick cannot fill the root with parts — the ninth failure finishes
the row `failed` with reason `attempts exhausted`.
