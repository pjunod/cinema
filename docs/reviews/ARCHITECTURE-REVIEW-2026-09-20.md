# Architecture review — where plurx stands after the month, and what to change

**Status:** review delivered · **Reviewed:** `main` @ `a1414368` (2026-09-20 04:24 UTC) ·
**Written:** 2026-09-20 · **Scope:** server, store/cluster, streaming pipeline,
Live TV, web/Apple/Android clients, build/CI/ops, and the last month's history
(3,860 non-merge commits since 2026-08-20; history back to 2026-08-18 was read)

Nine focused reviews ran in parallel (one per area plus one that read only the
git history), each anchored to `file:line`. I then re-verified every P0/P1 and
every "do this first" item below against the tree myself before writing it
down; where a claim rests on an agent's read alone it says LIKELY, not
CONFIRMED. The nine full reports (≈120 findings with quoted code, plus each
area's "already good" list and open questions) are in the companion
appendix; this document is the consolidated verdict, ranked.

How to read the ranking: **P0** = data loss / outage / security · **P1** =
user-visible stall, quality regression, or resource leak · **P2** = real
performance or maintainability win · **P3** = polish. Size: **S** < 1 day ·
**M** days · **L** weeks.

---

## 1. Verdict in one page

The month tripled the product code (54k → 162k product lines of Rust, 70 % of
the tree is now tests) and delivered real capability: a replicated store that
survives a voter loss, an encoded-VOD engine with a deterministic frame grid,
a playback-control protocol that fences every race it has met, Dolby Vision
P7→8.1 conversion, Live TV with ownership instead of elections, DVR, library
channels, three clients with shared contract fixtures. The parts that were
built as pure functions with tests beside them (`schedule.rs`, `prodsched.rs`,
`progress.rs`, the decision engine, the delivery planner, the web
`playback-policy.js`, the Android reducers) are stable: they took almost no
fix commits. Process discipline is also unusually good in places — commit
bodies read like RCAs, `#[ignore]` always carries a reason, zero `unwrap()` in
315k production lines.

Four things are wrong at the system level, and most of the ~120 findings hang
off them:

1. **The safety net has a hole where the tests should be.** Since 2026-09-10
   (`3cd127e2`) the only Rust job that runs before a merge is `cargo check`;
   no unit test and no workspace clippy executes on any PR, the full sweep
   (`ci.yml`) fires only on a `v*` tag or by hand, and the last tag is
   v0.3.0 from 2026-08-31 — 2,213 commits ago. Nothing is scheduled. Your
   standing instruction is "fast lane, then merge"; the fast lane no longer
   runs a test, so the instruction is being followed and the intent is not.
   Twenty red tests were carrying three live production defects on 09-17
   (#356); a Windows-port refactor on 09-13 silently un-piped child stdout
   and **two of its three regressions are still on `main`** (§2.1).
2. **The hot paths are built from defaults that were never sized.** Direct
   play and every HLS segment stream in 4 KiB chunks, one blocking-pool hop
   each; the HTTP listener has no timeouts at all; the encoded-VOD producer is
   SIGKILLed and respawned every control beat in steady state; text-subtitle
   burns spawn `fc-list` + `fc-conflist` per *segment*; the encoder runs
   1-pass ABR with B-frames disabled; every audio transcode is stereo AAC;
   there is no deinterlacer on the file path; the tone-map chain infers peak
   luminance. None of these was a decision — each is a default nobody
   revisited once the lifecycle machinery around it became the focus.
3. **The cluster pays consensus for things that do not need it, and reads
   nothing locally.** 225 `query_consistent` call sites (plan baseline 85);
   every authenticated request is a leader round trip; `bounded_replica_reads`
   defaults to `false`; the watched-outbox drains with a 1 Hz raft write on
   every voter forever; the "Replicated-ephemeral / Raft KV with TTL" tier in
   ARCHITECTURE §2.2 does not exist in code; the snapshot is a `VACUUM INTO`
   inline on the state-machine writer every 10k entries; and there is **no
   backup or restore for an activated cluster** — the documented gap in
   OPERATIONS.md is still open.
4. **Four files hold 65k product lines and absorb 40 % of all fixes.**
   `transcode.rs` 47k lines (155 fixes), `http/hls.rs` 28k (109),
   `playback_control.rs` 30k (84), `vodserve.rs` 15k (67),
   `cluster/membership.rs` 17k at **75 % fix density**; on the clients
   `PlayerController.swift` 9.5k (32 fixes, nine independent generation
   counters) and Android `Controller.kt` 4.3k (23 fixes, never instantiated by
   a test). The fixes are lifecycle-race fixes, and they recur because the
   boundaries are "which file the author was in", not "which state machine".
   681 `#[cfg(test)]` seams are woven into shipping structs, so the lifecycle
   the tests pin is not byte-for-byte the one that ships.

The single most useful reframing: **the month optimised for proof-by-test and
proof-by-ledger, and the incidents that reached the fleet were all found by a
human reading a log or a render.** Fragment-index worker dead 72 h with zero
write errors (`120a3d29`); M6 prepared handoff "enabled by default" for a week
and unreachable by any client; Live TV shipped twice with a start path that
could not succeed; GPU tone-map "never validated on any node". Every one of
those had a one-line production counter that would have said so. §5 proposes
that each effort's exit criterion becomes an observed-in-production number.

---

## 2. Do this first — ten items, all verified, most under a day

Ranked by (user harm × confidence) ÷ size. Each is CONFIRMED against `main`
unless marked.

### 2.1 `output_job_owned` never pipes the child's output — DV Profile 5 is refused and the media-origin probe is dead (P1, S)

`crates/plurxd/src/process_control.rs:39-44` spawns and calls
`wait_with_output()` without ever setting `Stdio::piped()`, so `stdout` and
`stderr` come back empty for every caller that did not pipe them itself.
`2e3a3bb5` (09-13, "feat(windows): complete native server runtime") replaced
nine `.output()` calls — which pipe by default — with this helper. #356 fixed
one caller (`pipeprobe.rs:208-222`, the GPU tone-map validation: every HDR
transcode had fallen back to software x264). Two more are still broken:

- `ffmpeg.rs:2660-2695` `dovi_probe_output` reads `output.stdout` for
  framemd5 lines → always `Err("produced no frame hashes")` →
  `transcode.rs:14322` `require_dovi_renderer` refuses with "this source did
  not prove that Dolby Vision RPU application changes pixels" and **caches the
  false result per file** in `dovi_proofs`. Every DV Profile 5 transcode has
  been refused since the 09-14 deploy.
- `transcode.rs:9264-9295` `probe_media_origin` parses `out.stdout` → always
  empty → falls back to the requested start for every copy-video HLS start at
  `start_seconds > 0`; the frozen presentation then claims the keyframe landed
  exactly at the request, which is what the fallback log line says it cannot
  know.
- `subtitles.rs:1446,1749`, `live_tv.rs:7403`, `ffmpeg.rs:2437,2513,2859`
  read `stderr` for their error text → every one of those messages is now
  empty ("live-TV FFmpeg graph failed: ").

Fix: make `output_job_owned` pipe both streams (that is the `Command::output`
contract every caller assumed) and add a unit test that spawns `/bin/echo`
through it and asserts non-empty stdout. The `dovi_proofs` cache is
in-memory, so the deploy clears it.

### 2.2 No Rust test runs before a merge, and none is scheduled after (P0, S)

`.github/workflows/main-fast-lane.yml:111-131` "fast Rust gate" = `make
effort-rust-check` (`Makefile:76-77`: fmt-check + `cargo check --workspace
--all-targets`) + `hiqlite-vendor-clippy`. `ci.yml:6-9` fires on `tags: v*`
and `workflow_dispatch` only; the sole cron in `.github/workflows/` is the
weekly `rust-audit`. `lint.yml:15-16` says "Main pull requests run Clippy in
main-fast-lane.yml" — they do not. The 853-file regression ledger maps 511
corrective commits to a `rust-gate` check that no PR runs.

This also breaks the fast/full model you set: the "fast lane" you meant was
`make unit` (~2–4 min warm per DEVELOPMENT_PIPELINE §4), and the "another
process fixes full-suite failures in batches" has no input because nothing
produces failures. Fix: put `cargo test -p plurxd --bin plurxd` +
`cargo test -p plurx-core --lib` and workspace `clippy -D warnings` back in
the fast Rust gate (the compile is already paid; the tests add the 2–4 min),
and add a scheduled `ci.yml` run on `main` every 4 h with
cancel-in-progress so the batch process has something to batch. Keep the
heavy cluster lanes tag/dispatch-only as they are.

### 2.3 Back up the replicated store (P0, M)

`OPERATIONS.md:351-356, 392-401`: "There is no automated quorum-aware backup,
restore, or permanent-majority recovery path for an activated cluster. No
active milestone owns one." `Cargo.toml:38` builds hiqlite without its
`backup` feature; Ansible copies the frozen pre-activation `plurx.db`, which
is not a restore point. Users, tokens, settings, watch state and the
catalogue exist only in three `hiqlite/` directories on one LAN, behind a
hand-rolled 1,009-line `migrate_schema` that moved 35 versions this month and
needed four startup hotfixes (`e26161a5`, `f34aecf5`, #304, #338).

Fix: enable the vendored `backup` feature (make `s3` optional in the fork —
see §4.3), add `plurxd backup --output` (leader `VACUUM INTO`, run off the
writer per §3.2), a documented restore-as-single-voter-then-rejoin, a nightly
leader-singleton schedule through the existing job lease, and
`plurx_backup_last_success_seconds` on `/metrics`.

### 2.4 Media bodies stream in 4 KiB chunks through a blocking-pool hop per chunk (P1, S)

`http/stream.rs:2940,2957` and `http/hls.rs:13402,14012` use
`ReaderStream::new` (tokio-util default 4096 B); each read is a
`tokio::fs` `spawn_blocking` round trip, and the HLS pump additionally sends
every chunk through `mpsc::channel(1)` (`hls.rs:9900`) and waits for a
oneshot ack. An 80 Mb/s UHD direct play is ~2,500 blocking-pool hops per
second per viewer; a 4 MB segment is ~1,000 read→channel→hyper handoffs. The
right idiom is already in the tree: `internal_media.rs:116` and
`offline.rs:1192` use `with_capacity(256 * 1024)`. Fix the four sites, and
ack per byte-budget rather than per chunk in the pump. This is the "goal
state" path in ARCHITECTURE §3 and it is currently the most expensive way the
daemon has to serve a file.

### 2.5 The HTTP listener has no timeouts, limits or compression (P1, S)

`main.rs:2418` `axum::serve(...)` with no `.timer()`, so hyper's own 30 s
header-read default is silently disabled (`hyper-1.10.1
common/time.rs:72-76`); `http/mod.rs:629-643` has only `TraceLayer` and the
capacity gate — no `TimeoutLayer`, `ConcurrencyLimitLayer`, `LoadShed`,
request-id or `CompressionLayer` anywhere in `crates/`. SECURITY.md defers to
a reverse proxy, but bare LAN is the documented normal deployment. The web
app is 2.6 MB raw / 0.87 MB gzip across 71 synchronous HTTP/1.1 requests
before `boot()` runs (`web.rs:207-217` sets no `Content-Encoding`;
`flate2` is already a direct dependency). Fix: build the server with
`hyper_util::server::conn::auto::Builder` + `TokioTimer`,
`header_read_timeout(15s)`, h2 keepalive; `TimeoutLayer` on the JSON route
groups (not on streaming routes); precompress `WEB_ASSETS` once at startup in
a `LazyLock` beside `ASSET_HASHES` and serve gzip on `Accept-Encoding` — do
not wrap the whole router in `CompressionLayer`, it would touch `/hls/*`.

### 2.6 Encoded VOD kills and respawns ffmpeg every control beat in steady state (P1, S–M)

`vodserve.rs:6316-6322` turns `Step::Stop` (SIGSTOP) into
`Step::Terminate { IndefiniteHold }` for every encoded rendition when the
ahead window is full; `prodsched.rs:372-386` resumes as soon as one segment
of gap opens, with no low-water mark (the hysteresis at `:214-237` covers
only the working-set budget). The frontier is the playhead from control, on a
5 s cadence. So after the first ~180 s a two-hour film is ~1,400 process
launches, each paying `recipe_engine_is_current` (a stat of the whole ffmpeg
`.so` closure, `ffmpeg.rs:1693-1699`), a store read, decoder init, a 2 s
preroll decode, encoder warm-up — and x264 ABR restarts its rate-control
history at every boundary, so the pumping in §3.1 recurs every few seconds.
VOD-ENCODING.md's own measurement is 7.6 s of preparation per restart.
`prodexec.rs:151-164` documents the opposite intent ("resuming costs nothing
where restarting costs a reposition"). Fix: SIGSTOP encoded renditions like
copy ones and release only when another rendition is actually queued in
`Admissions` (`vodencode.rs` already exposes `is_waiting`); add a low-water
mark so a producer runs in ≥90 s bursts. `prodsched.rs` is pure and
well-tested — add the cases there first.

### 2.7 Text-subtitle burns spawn `fc-list` and `fc-conflist` per producer launch **and per segment** (P1, S)

`vodserve.rs:7483` calls `recipe_engine_is_current` inside `materialize()`
(per segment) and again at `:6741` per spawn; it reaches
`EncodedEngine::is_current` (`ffmpeg.rs:1612-1621`) which calls
`font_render_engine_inner()` (`:1806-1850`: two process spawns plus a
synchronous `std::fs::metadata` on every font file, on a tokio worker) every
time, nothing memoised. A producer at 3× realtime materialises a segment
every ~0.7 s. Attest the font closure once per capture, re-check on a timer
(60 s) or inotify, and move `engine_objects_are_current` stats to
`spawn_blocking` or a 1 s TTL. Combined with 2.6 this is also paid per
respawn.

### 2.8 tvOS never sets `AVDisplayCriteria`, so Match Frame Rate / Match Dynamic Range cannot engage (P1, S)

The Apple player is a bare `AVPlayerLayer` (`PlayerSurface.swift:310-311`,
chosen to avoid `AVPlayerViewController`'s LIVE treatment) and there is no
`AVDisplayCriteria` / `preferredDisplayCriteria` / `avDisplayManager` anywhere
in `clients/apple/Sources`. Apple's contract is that `AVPlayerViewController`
applies `asset.preferredDisplayCriteria` and a layer-based app must set
`UIWindow.avDisplayManager.preferredDisplayCriteria` itself. An Apple TV in
Apple's default "4K SDR + Match Content" configuration therefore keeps the
HDMI link at SDR/60 Hz: 24p film gets 3:2 pulldown and the HDR10/DV
bitstream the server preserves is tone-mapped to SDR *inside the Apple TV*
while the playback-info badge says "HDR10 delivered". A box hard-set to
"4K Dolby Vision" hides this, which is likely why it has never been seen.
Fix: after `open()` (`PlayerController.swift:4695`) set
`AVDisplayCriteria(refreshRate:videoDynamicRange:)` from the decision's frame
rate and delivered range when `isDisplayCriteriaMatchingEnabled`, reset to
`nil` in `stop()`; same for the Live TV and library-channel players. Verify
on the bedroom Apple TV in the default output mode — that is the GPT test.

### 2.9 Android: no refresh-rate matching, and a byte budget that starves 4K on TV heaps (P1, S–M)

No `setVideoChangeFrameRateStrategy` / `preferredDisplayModeId` /
`Surface.setFrameRate` anywhere in `clients/android/app/src/main`; Media3's
default `ONLY_IF_SEAMLESS` never switches an HDMI TV from 60 to 24, so every
film judders (Plex, Jellyfin, Kodi and the Shield launcher all switch).
`PlaybackLoadControl.kt:15-33` sets `targetBufferBytes =
min(memoryClass/8, 64) MiB` with prioritize-time **off** on every player —
16 MiB on a 128 MB class box, 1.7 s of an 80 Mb/s UHD remux, against an 8 s
stall tracker that then reopens the session. It was an OOM fix for two primed
pipelines on the Lenovo and is paid always, not only while a successor is
primed. Fix: TV-only `STRATEGY_ALWAYS` (API 31+) plus `preferredDisplayModeId`
for older TVs, switched before `prepare()`; `largeHeap` + size from
`largeMemoryClass`, floor the incumbent at 48 MiB and shrink only the
successor while priming.

### 2.10 Apple: no `AVAudioSession` interruption or route-change handling; the stall detector "recovers" every system pause (P1, S–M; LIKELY on device)

`PlayerController.swift:2610-2612` sets the category and nothing observes
`interruptionNotification` or `routeChangeNotification` in any of the three
player stacks. The recovery monitor (`:5120-5127`) keys on `wantsPlayback` and
a stationary clock, so a phone call, Siri, or pulling the AirPods out pauses
AVPlayer, six seconds later the nudge calls `play()` (film resumes out of the
speaker — the exact thing Apple's route-change guidance exists to prevent),
twelve seconds later a same-delivery reopen creates a second server session,
and after a one-minute call the viewer is on a terminal "Playback stopped."
Fix: observe both notifications, gate `shouldMonitor` on a `systemPaused`
flag, resume only on `.shouldResume`, set `wantsPlayback = false` on
`.oldDeviceUnavailable`, pass `mode: .moviePlayback`. Two-minute experiment
on the iPhone to confirm.

---

## 3. Findings by area

Each area lists what to change in priority order. The appendix has the full
finding text; only the evidence anchor and the decision are repeated here.

### 3.1 Video quality — the encoder is running defaults nobody revisited (all P1, mostly S)

| # | What | Evidence | Change |
|---|---|---|---|
| Q1 | Default rate control is 1-pass ABR on every encoder family; the swept QVBR/CRF mode is opt-in | `encoder.rs:106-110` `RateMode { #[default] Bitrate, Quality }`; `transcode.rs:12552-12562`; `bitrate_for_height` never consults `file.bitrate` | Make `Quality` the default for x264 (`-crf 23` + VBV) and QSV (swept at 22); cap `maxrate` at `min(ladder, 1.2 × source)`. Keep Bitrate for un-swept families until `scripts/bench rate-control` runs on them |
| Q2 | Encoded VOD disables B-frames (`-bf 0`) on every encoder | `vod.rs:270-275` | 10–20 % efficiency at equal CRF on x264, more on NVENC/QSV. Keep B-frames with `-movflags +negative_cts_offsets`; the IDR grid is unchanged; validate with `establish_or_verify` |
| Q3 | CPU tone-map: no explicit `peak`, primaries converted after the curve, 8-bit output without dither | `mod.rs:1040-1056` | Store MaxCLL/max-luminance at scan and emit `peak=`; `zscale=p=bt709` before `tonemap`; `dither=error_diffusion`; `bt2390` where the build has it |
| Q4 | No deinterlacer on the file path; scanner does not record `field_order` | only `live_tv.rs:6135` has `bwdif`; `decode_facts.rs:3343` list lacks `field_order` | Capture `field_order`; `bwdif` before `scale` when interlaced; `vpp_qsv=deinterlace=2` / `deinterlace_vaapi` on hardware graphs |
| Q5 | Every full transcode is stereo AAC 160 k, no `-ar`, no downmix matrix; no EAC3/AC3 exists although ARCHITECTURE §3 promises it | `TranscodeOptions::default` `mod.rs:895-913`; `hls_args_inner` `:1661-1666` | Carry the profile's max channels/codec into `TranscodeOptions`; `eac3 640k` when claimed, else `aac -ac 6 320k`; `-ar 48000` on lossy; explicit centre −3 dB stereo matrix |
| Q6 | NVENC is `main` profile with no B-frames/AQ/lookahead and never zero-copy; VideoToolbox similar | `encoder.rs:436-444`; no CUDA pipeline in `pipeline.rs` | `-profile:v high -bf 3 -b_ref_mode middle -spatial-aq 1 -temporal-aq 1 -rc-lookahead 20`; `Pipeline::Cuda` behind the boot probe. Only matters if an NVENC node exists — open question |
| Q7 | Master playlist advertises the *source* BANDWIDTH/RESOLUTION for transcodes and omits CODECS on SDR variants | `hls.rs:12413-12461` | RFC 8216 §4.3.4.2 says peak of the variant; AVPlayer filters on it and the Apple panel mis-grades transcodes as "critical". Emit the rung's numbers and `CODECS` on every variant |
| Q8 | Rolling path: no `-sc_threshold 0` so scene-cut IDRs drift off the 2 s grid; HEVC Main10 in MPEG-TS; `tonemap_opencl=hable` | `mod.rs:1651-1653, 1696-1697`; `pipeline.rs:367` | Bundle of one-liners |
| Q9 | Live TV: encode bitrate uses source fps while `bwdif=send_field` doubles it — 1080i sports get half the bits; captions dropped on VAAPI, no `CLOSED-CAPTIONS` rendition | `live_tv.rs:6110-6135`; `:5992` `-sn -dn` | Compute output fps in the planner; `-sei +a53_cc`; two-line master with `EXT-X-MEDIA:TYPE=CLOSED-CAPTIONS` |
| Q10 | Web: `enableWorker:false` (main-thread TS demux); a single fatal media error skips hls.js's recovery ladder and permanently downgrades to transcode; capability probe asserts H.264 with no ceiling | `player.js:560`, `prepared-replacement.js:275`; `player.js:756-795` (no `recoverMediaError` call anywhere) | Delete the two lines (the vendored build already falls back inline on worker failure); one `recoverMediaError()` then `swapAudioCodec()` before the transcode rescue |
| Q11 | Apple claims no `flac`/PCM so lossless audio is re-encoded; Android under-claims Vorbis, probes 30 fps only, never states HEVC Main10 | `Caps.swift:142`; `CapsPolicy.kt:260-284`, `Caps.kt:157` | One caps-matrix fixture each |

The clients' *policy* layers and the copy/remux path are in good shape (see
§6). The gap is entirely in the encode arguments and the master playlist.

### 3.2 Store and cluster — consensus where none is needed, nothing local

| # | Sev | What | Evidence | Change |
|---|---|---|---|---|
| S1 | P1 | Default read path is a leader round trip for everything: `user_for_token` per authenticated request (122 of 205 routes), catalogue reads, settings, session routes; 225 `query_consistent` sites vs plan baseline 85; `bounded_replica_reads` default `false` so `CatalogueReader` returns `None` | `hiqlite.rs:3980-3997`; `extract.rs:627-636`; `config.rs:170`; vendored `client/query.rs:11-20` ("very expensive … pauses the raft") | Default `bounded_replica_reads=true`; give `AuthUser` the revocation-fenced per-node cache already built for `CacheOnlyAdminUser` (`extract.rs:30-105`); route the remaining catalogue reads through `CatalogueReader`; a test that fails when `query_consistent` sites grow without a justification comment. Also fixes ARCHITECTURE §1's "every node serves reads", which is not true today |
| S2 | P1 | Snapshot every 10k log entries is a `VACUUM INTO` inline on the state-machine writer; with `probe_json` at 5–30 KB/row the DB is ~10 KB × files, and a ≥8 s stall self-fences every 12 s session lease on every node | `migration.rs:2327` `default_raft_config(10_000)`; vendored `writer.rs:503-535, 763-767`; `media_sessions.rs:89-90,133-134` | Measure `plurx_raft_snapshot_seconds{operation="build"}` first; raise `logs_until_snapshot` to 100k–500k; patch #16: build from a read connection off the apply path (rqlite does); move `probe_json` to a side table (S14) |
| S3 | P2 | Watched-outbox drain is an unconditional 1 Hz `UPDATE … RETURNING` raft proposal on every voter forever — 259k no-op fsynced entries/day driving S2's cadence | `watched.rs:192-199`; `hiqlite_durable.rs:702-722`; `main.rs:2375-2377` | Local read-before-write, skip when `MONARR_URL` unset, 5–10 s idle backoff, leader-singleton via the job lease. Audit the takeover loop (`media_sessions.rs:4353-4377`: two consistent `get_setting`/2 s per node with the feature disabled) the same way |
| S4 | P1 | No backup/restore for an activated cluster | §2.3 | §2.3 |
| S5 | P2 | A failed snapshot build is fatal to the raft node; storage floor is a fixed 512 MiB regardless of DB size while `VACUUM INTO` needs ≥ DB size | `membership.rs:144`; openraft `raft_core.rs:1381` | Floor = `max(512 MiB, 1.5 × DB)`; alert on `snapshot_seconds{outcome="error"}` |
| S6 | P2 | SQLite backend: ~70 read-only methods (incl. `user_for_token` and every Home rail) run on the single writer mutex; `user_for_token`'s conditional UPDATE takes the WAL write lock every call; `READ_CONNS = 2` | `sqlite/users.rs:273-299`; `sqlite/mod.rs:1300` | `with_read` for every DML-free closure; split the touch into read + rate-gated write (hiqlite already does); `READ_CONNS = min(8, cores)` |
| S7 | P2 | Home/Next-Up/library-page queries are O(catalogue) per request; no `ANALYZE`/`PRAGMA optimize` ever; missing `(library_id, kind, sort_title/added_at/year)`, `(kind, parent_id)`, `tmdb_id` indexes; search does `rowid NOT IN (SELECT rowid FROM classification_fts)` — a full FTS scan per query | `media.rs:856-896, 970-1000, 1021`; `watch.rs:488-520` | Indexes; rank `(id, key)` then join after `LIMIT`; `NOT EXISTS` on the PK; `PRAGMA optimize` at close, `ANALYZE` after big scans; keyset pagination |
| S8 | P2 | The "Replicated-ephemeral / Raft KV with TTL" tier in ARCHITECTURE §2.2 does not exist; hiqlite is built without `cache`; renewals and heartbeats are durable SQLite writes | `Cargo.toml:38` | Either correct §2.2 or adopt the tier for routes/heartbeats and keep only ownership fences durable |
| S9 | P2 | No runtime guard on cross-node clock skew although lease expiry and takeover are decided on wall clocks; only prose requires ≤250 ms | `media_sessions.rs:3888-3899, 4384-4388` | Derive skew from heartbeat `last_seen_at`, export it, refuse takeover / drop readiness above 2 s (CockroachDB `--max-offset` pattern) |
| S10 | P2 | Two hand-maintained SQL implementations drift independently; the PR lane exercises hiqlite tests only when `scope.cluster_auth` is true and `points.toml` omits 7 `hiqlite_*.rs` / 20 of 24 `sqlite/*.rs`; the `$N`-placeholder census is hiqlite-only although the SQLite twin was the last one to break (`cfba5ff6`) | `points.toml:448-494,550-561` | Add the paths (one line); run the placeholder validator on `?N`; long-term one SQL source per method |
| S11 | P3 | SQLite housekeeping at 2006 defaults: only four pragmas, `prepare_cached` used twice, permanent mutex poison, no boot `quick_check`, `files` trailing columns after `probe_json` | `sqlite/mod.rs:1352-1355, 1583-1584` | `cache_size=-65536 temp_store=MEMORY mmap_size=256MiB journal_size_limit=64MiB`; `into_inner` on poison; `file_probes` side table |

### 3.3 Server core

| # | Sev | What | Evidence | Change |
|---|---|---|---|---|
| C1 | P1 | 4 KiB media bodies | §2.4 | §2.4 |
| C2 | P1 | No listener timeouts/limits | §2.5 | §2.5 |
| C3 | P1 | Scanner walks the tree with synchronous `WalkDir` and per-file `std::fs::metadata` on a tokio worker; zero `spawn_blocking` in `scan/` | `scan/mod.rs:494, 358-368` | Walk in `spawn_blocking` in pages of ~256; `tokio::fs::metadata` for per-file reads. On NFS this pins a runtime worker for the whole scan |
| C4 | P1 | Plex façade `section_all` is an N+1 over up to 5,000 items with no `X-Plex-Container-Start/Size` | `plex.rs:191-267` | Batched `files_for_items`/`child_counts` (helpers exist in `browse.rs:236/247`); honour container paging. Only matters if Kodi/PKC is in use |
| C5 | P2 | TMDB/AniList clients have no timeout; enrichment runs under a leased singleton so a hang blocks every node | `tmdb.rs:140-144`, `anilist.rs:59-62` | `connect_timeout(5s).timeout(30s)` + per-item deadline |
| C6 | P2 | Image serving SHA-256s the whole file on the runtime per request, 503s local hits above 8 in flight, no `ETag`; backdrops stored at `original` | `images.rs:33, 87-89, 255-263, 838-847`; `metadata/mod.rs:43-44` | Validate by size+mtime+inode (already computed), separate local-read from peer-fetch permits, `ETag` = digest, `w1280` backdrops or `?size=` |
| C7 | P2 | Logout/user mutations are a cluster-wide two-phase protocol behind a process-wide `try_lock`; the second concurrent one gets 503 while the client clears its bearer anyway | `auth.rs:100-109`; `extract.rs:221-228`; `internal_auth_revocation.rs:265-354` | Plain `DELETE FROM tokens` + best-effort async peer notify, or `lock_owned` with a timeout |
| C8 | P2 | No login throttle per account/IP; tokens never expire (no `expires_at` on `tokens`); `?token=` accepted on every route | `auth.rs:45-91`; `sqlite/mod.rs:72-78`; `extract.rs:547-577` | `(username, ip)` backoff map (`ConnectInfo` is wired); idle expiry on `last_seen_at` (90 d) + prune; a devices list |
| C9 | P2 | 1 s route-cache TTL ⇒ ≥1 consistent read/s per active HLS session behind one global `tokio::Mutex<HashMap>` | `media_sessions.rs:107, 1485` | Cache positives to `min(lease_expires, now+TTL/2)`; shard like `route_queries` |
| C10 | P3 | No HTTP RED metrics, no request ids, access log at DEBUG, no JSON log format, ANSI escapes into journald, no panic hook (task panics never reach `logbuf`) | `http/mod.rs:629-638`; `main.rs:1542-1560` | `MatchedPath` counter+histogram middleware, `SetRequestIdLayer`, `with_ansi(is_terminal())`, `PLURX_LOG_FORMAT=json`, panic hook → `tracing::error!` |
| C11 | P3 | Direct play carries no `ETag`/`Last-Modified`/`Cache-Control` and treats `If-Range` as a full 200, contrary to ARCHITECTURE §3 | `stream.rs:2828-2832, 2941-2968` | Strong validators, honour `If-Range`, skip the Store read on a validator match |

### 3.4 Live TV and channels

| # | Sev | What | Evidence | Change |
|---|---|---|---|---|
| L1 | P1 | DVR rule expansion reads the guide through the 2 MiB *response* clipper — a 14-day horizon over 40–60 channels is 10–20 MB of JSON, so series rules only see the first day; and the whole guide is cloned + JSON-serialised twice per 15 s tick under the guide mutex | `dvr.rs:753-760, 848-857`; `guide.rs:222-279` | Give the scheduler an `Arc<LiveTvGuide>` view; apply the byte bound only in the HTTP handler; clip per channel by binary search; regression with a day-13 airing |
| L2 | P1 | Every live session does a leader-consistent read of the whole settings table once per second; a ~6 s raft hiccup ends the stream with text blaming the tuner | `live_tv.rs:5469-5475, 2740-2747`; `hiqlite.rs:3499-3509` | Publish `LiveTvConfig` via a `watch` fed by the settings write path; fence in memory; read failure = "unknown, continue" |
| L3 | P2 | Ingress builds a new `reqwest::Client` and runs a membership SQL query for **every** relayed playlist/segment (2/s per remote viewer) | `http/live_tv.rs:1093, 1121, 1356-1385`; `peer_transport.rs:50-58` | One `PeerTransport` in `AppState`; cache `owner_peer` per membership epoch |
| L4 | P2 | One tuner + one ffmpeg per *viewer*, default ceiling 2; the DVR fan-out primitive exists but viewers do not use it | `live_tv.rs:1253-1268, 267-269, 1402-1405` | `LiveTransport` keyed by `(device, channel)` fanning TS to N pumps; optionally share ffmpeg per plan |
| L5 | P2 | Library-channel guide/resolve clone the whole generation (≈720 KB) on every LRU hit, re-validate O(n) per call, up to 1,000× per guide page, under one process-wide `std::sync::Mutex` | `http/library_channels.rs:1299-1311, 941-963`; `library_channels.rs:993-1047` | Cache `Arc<Generation>`, validate once at insert, advance by ordinal |
| L6 | P2 | Every start spends a fixed 3 s collecting a prefix and ~0.5–2 s ffprobing it although the channel's format is cached for 20 min (docs: first segment 5.5–7 s) | `live_tv.rs:136-137, 5201-5296, 5632-5660` | Spawn immediately on a warm cache and let the stderr format watcher demote |
| L7 | P2 | Bitrate/captions | §3.1 Q9 | — |
| L8 | P2 | `live_tv.rs` is nine modules in one 468 KB file; three parallel HLS playlist parsers in the tree | `transcode.rs:1107`, `renditiondir.rs:162`, `live_tv.rs:6487` | Mechanical split under `live_tv/`; one `hls_playlist` parser in `plurx-core` |
| L9 | P3 | A session whose scratch cleanup fails is never retired and counts toward the tombstone cap that refuses all starts; producer polls scratch every 250 ms with `read_dir` + per-file metadata; no `EXT-X-PROGRAM-DATE-TIME`; misleading "capability expired" after owner restart | `live_tv.rs:5071-5080, 6362-6477, 4784-4786` | Always retire; inotify or playlist-mtime gate; `+program_date_time`; distinguish `unknown` |

The 1 s uniform-cadence fix and the persisted guide are correct — the two
documented Live TV incidents were fixed at the root, not the symptom.

### 3.5 Web client

| # | Sev | What | Evidence | Change |
|---|---|---|---|---|
| W1 | P1 | 2.6 MB uncompressed over 71 sequential HTTP/1.1 requests | §2.5 | gzip at startup; `<link rel=preload>` for the largest rows |
| W2 | P1 | `hls.min.js` is neither hashed nor `no-cache` (`max-age=604800`, no ETag) while app code subclasses its internals — a 7-day version-skew window on the one library whose private API is depended on | `web.rs:228-238`; `player.js:558-564` | Add it to `WEB_ASSETS` so it gets the hash + `immutable` |
| W3 | P1 | Every non-VOD seek reopens the server session — a ±10 s nudge on a transcode is a new ffmpeg; PLAYBACK.md calls this "open work" | `transport.js:733-773`; `decode-tiers.js:1122-1165`; `PLAYBACK.md:1573-1588` | Seek locally when the target lands in `v.buffered` or the published fragment range minus a 1.5 s holdback — the Apple `seekRoute` rule; put the decision in `playback-policy.js` for the Node matrix. Android has the same gap |
| W4 | P2 | Worker off / no `recoverMediaError` | §3.1 Q10 | — |
| W5 | P2 | Seven sidecars (266 KB) are `no-cache` with no validator, re-downloaded in full per navigation; 196 KB of base64 fonts inside the render-blocking stylesheet (44 % of critical CSS) | `web.rs:241-328`; `app.css:2` | Hash them via the tag rewrite; serve `.woff2` as rows with `preload` |
| W6 | P2 | No CSP or security headers, 398 inline `on*=` handlers, bearer in `localStorage` and in every `<img src>` as `?token=` | `web.rs:197-199`; `core/api.js:70` | Now: `nosniff`, `Referrer-Policy`, `CSP: frame-ancestors 'none'; base-uri 'none'; object-src 'none'`. Then delegate handlers to one `data-action` listener and ship `script-src 'self'`. Escaping discipline (`esc()`) was checked and is consistent |
| W7 | P2 | No global error reporting; after the split a load-time throw is silent and nothing reaches `/client-log` | only `transport.js:527` handles `error`, on the `<video>` | `window.onerror`/`unhandledrejection` → `clientLog` with a cap; a boot sentinel banner |
| W8 | P2 | Structure: 62 scripts, one global scope, no types, only `node --check`; 166 of 427 commits were fixes, dominated by ownership/generation races on a shared untyped `PLAYER` bag; `play()` is 479 lines, `attachHls` 378 | `decode-tiers.js:606`; `player.js:499` | **Not** a bundler/ESM migration. `tsc --noEmit --allowJs --checkJs` in CI with a `jsconfig.json` listing the rows in served order (TypeScript models scripts as one global scope, which is the runtime truth) + ESLint `no-undef`; one JSDoc `@typedef Player`; then split `play()`/`attachHls()` along their own comment seams |
| W9 | P3 | Library grids repaint the whole page per 200-item batch, no poster size variants; progress heartbeat continues while paused; no MediaSession/media keys/TV back-key codes; poster hover ignores `prefers-reduced-motion` in one layout | `library-grids.js:24-159`; `decode-margin.js:517-521`; `stats.js:586-613`; `app.css:147` | Small fixes |

### 3.6 Apple client

| # | Sev | What | Evidence | Change |
|---|---|---|---|---|
| A1 | P1 | No display criteria on tvOS | §2.8 | §2.8 |
| A2 | P1 | No audio-session handling | §2.10 | §2.10 |
| A3 | P2 | `PlayerController.swift` 9,548 lines, 250 methods, **nine** independent generation counters, 58 hand-written fence conjunctions, 39 unstructured `Task {}`, 32 fix commits (12 on one day) | `PlayerController.swift:1619+` | One `struct Attempt {lifecycle, open, viewerAction, item, session}` captured once per continuation; then split by the seams the file already draws. Do after A1/A2 |
| A4 | P2 | Three independent AVPlayer stacks; the main one observes only `status` + end and never `FailedToPlayToEndTime`, `NewErrorLogEntry`, `timeControlStatus`; error classification reads `errorLog()?.events.last`, often a benign subtitle entry | `:6908-6977, 6990, 7258` | One `AVPlayerItemObserver` emitting a typed `AsyncStream<PlayerEvent>` for all three |
| A5 | P2 | `LibraryView` eagerly pages the whole library, re-sorts the merged array on the main actor after every page, filters three times per render — 6,000 titles = 30 main-thread sorts and 24k `localizedCaseInsensitiveContains` per keystroke | `AppModel.swift:611-644`; `LibraryView.swift:14-94` | Page on demand near the grid end; drop client re-sort; memoise with a 150 ms debounce |
| A6 | P2 | 25 / 50 / 100 ms `Task.sleep` poll loops on the main actor (~480 wakeups per quality change); a 2 s status poll for the life of every session including backgrounded audiobooks | `PlaybackControlSession.swift:568`; `PlayerController.swift:7990-7997, 6637-6713, 5011-5047` | KVO publishers / continuations; status poll backs off to 10 s with the panel closed |
| A7 | P3 | Now Playing rewritten at 2 Hz (an XPC each); tvOS has no Now Playing / remote commands at all; no `AVRoutePickerView`; no scrub thumbnails; Swift 5 mode with no strict concurrency and one `@unchecked Sendable` (`Session`) with unguarded `origin`/`token` | `:6823, 8832-8847`; `project.yml:14-18`; `Session.swift:8-14` | Update on state change; enable remote commands on tvOS; `SWIFT_STRICT_CONCURRENCY: complete` as warnings, then errors |

### 3.7 Android client

| # | Sev | What | Evidence | Change |
|---|---|---|---|---|
| D1 | P1 | Buffer budget / no frame-rate matching | §2.9 | §2.9 |
| D2 | P1 phone / P2 TV | Backgrounding never pauses VOD (no ON_STOP on the player); no `MediaSessionService`/foreground service or wake mode for audio; `keepScreenOn` unconditional | `PlayerScreen.kt:1089-1097, 1314`; `Controller.kt:2640-2647`; manifest | ON_STOP + not PiP → pause; audio-only → `MediaSessionService` + `WAKE_MODE_NETWORK`; `setWakeMode` instead of `keepScreenOn` |
| D3 | P2 | Live TV treats `ERROR_CODE_BEHIND_LIVE_WINDOW` as fatal; AudioTrack failures (5001/5002/5004) never enter the compatibility ladder although the route-aware probe exists for exactly that; node failover fires on 401/404/410 because the "filtered at the call site" comment describes code that does not exist | `LiveTvPlayer.kt:481-488`; `PlaybackPolicy.kt:77, 101-120`; `Controller.kt:704-707, 2280-2331` | `seekToDefaultPosition(); prepare()` once per attach; add 5xxx to `isCompatibilityPlaybackError` and re-snapshot caps; gate failover on status |
| D4 | P2 | Three of four `ExoPlayer.Builder` sites skip audio focus, becoming-noisy, tunneling and decoder fallback | `LiveTvPlayer.kt:164-167`, `LibraryChannelPlayer.kt:65-68`, `OfflineDownloads.kt:391-395` | One `PlurxPlayerBuilder(context, live)` |
| D5 | P2 | "Open in…" hands the account bearer to arbitrary apps via `?token=`; bearer in plaintext DataStore included in cloud backup | `DetailScreen.kt:486-491`; `Session.kt:118-123`; `SettingsStore.kt:14,39,142`; manifest `allowBackup="true"` | Server-minted short-lived file grant; exclude `datastore/` from backup (one line) |
| D6 | P2 | One OkHttp dispatcher (5/host) shared by Coil and Retrofit — `/decision` queues behind 24 posters; whole-library download with per-page main-thread re-sort; `Controller.kt` (4,293 lines) never instantiated by any test; sideload path builds `assembleDebug` (no R8, `run-as` exposes the token, no Baseline Profile) | `Net.kt:23-38`; `AppViewModel.kt:674-681`; `Makefile:1713-1751` | Separate dispatcher; sort off-main / server sort; `PlayerPort` + JVM fake; release signing + Baseline Profile |
| D7 | P3 | `Application.onCreate` does two `runBlocking(IO)` reads for offline books TV never uses; 2 s session-status poll for the life of every session regardless of panel | `OfflineDownloads.kt:113-141`; `Controller.kt:2225-2256` | Gate + lazy; poll only while the panel is open |

---

## 4. Architecture and process — the structural findings

### 4.1 The god modules, and what to split them into

Fix density is the strongest single stability signal the history offers, and
it is concentrated:

| File | Lines (product) | Commits / fixes since 08-20 | Fix % |
|---|---|---|---|
| `crates/plurxd/src/transcode.rs` | 47,096 (27,573) | 294 / 155 | 52 |
| `crates/plurxd/src/http/hls.rs` | 28,507 (14,193) | 204 / 109 | 53 |
| `crates/plurxd/src/playback_control.rs` | 29,986 (14,533) | 188 / 84 | 44 |
| `crates/plurxd/src/vodserve.rs` | 15,011 (8,091) | 110 / 67 | 60 |
| `crates/plurx-core/src/cluster/membership.rs` | 17,169 (10,708) | 106 / 80 | **75** |
| `crates/plurxd/src/http/cluster_operations.rs` | 4,978 | 39 / 32 | **82** |
| `crates/plurx-core/src/store/hiqlite_sessions.rs` | 4,977 | 65 / 42 | 64 |
| `clients/apple/Sources/PlayerController.swift` | 9,548 | 72 / 36 | 50 |
| `clients/android/…/player/Controller.kt` | 4,293 | 66 / 29 | 43 |

45 files over 100 KB hold 71.5 % of all Rust. The same request crosses
`hls.rs` → `transcode.rs` (`authorize_response_publication`, three
`sessions.lock()` acquisitions) → `vodserve.rs`, with **two session
registries** (`TranscodeManager.sessions` and `VodServe.shared.sessions`)
carrying parallel `response_owner_is_live` / `promote_prepared_session` /
`segment_window` implementations. Helpers have already drifted: three ffmpeg
spawn implementations (the VOD one at `vodserve.rs:6778-6794` bypasses
`configure_ffmpeg_runtime` — the fontconfig `XDG_CACHE_HOME` fix and the
Windows job object do not apply to the primary path), two `is_progress_line`
copies where the loose one (`stream.rs:3409-3420`) swallows exactly the bsf
error the strict one's comment warns about, three frame-rate parsers.

The decomposition is written out in the streaming appendix (module by module
with line ranges: `producer/spawn.rs`, `producer/attempt_child.rs`,
`rolling/{segment_index,flow,prepublication,retirement,publication,session}.rs`,
`delivery.rs`, `session_request.rs`, `requests.rs`, `pretranscode.rs`,
`cache/offer.rs`, `rate_control.rs`, `manager.rs`; `http/hls/{create,release,
status,control,preparation,relay,playlist,subtitles,segment}.rs`;
`vod/{discovery,session,marker_prewarm,admission,driver,serve,control,init}.rs`).
Do it as `git mv` + `pub(crate)` re-exports first — the web-shell split
(#371) proved a byte-identical decomposition with a one-shot gate, and that
is the template — then unify the spawn path (fixes the drift), then, as a
separate step, the two registries. For `playback_control.rs`, make
`ControlState::accept_at` (a 240-line function with 14 early returns mutating
17 fields) a pure `(State, Request) → (State, Disposition)` step the way
`schedule.rs` already is. For `membership.rs`, extract a `NodeLifecycle`
transition function (etcd's membership shape). **Size L, but it is the
prerequisite for every other lifecycle fix landing without a new race.**

### 4.2 Test seams compiled into production types

681 `#[cfg(test)]` sites in `src/` are not test modules — they are fields and
branches inside shipping structs (`transcode.rs:3290-3293`
`wait_before_await_pause: Mutex<Option<Arc<Barrier>>>`, three pause fields on
the attempt supervisor, 208 in `transcode.rs`, 148 in `playback_control.rs`).
Struct layouts, lock acquisitions and `.await` cancellation points differ
between test and release, so the supervisor orderings the tests pin are
proven for a test-shaped state machine. The repo already has the right
precedent (`scratch-fault-injection` feature,
`PLURX_CLUSTER_ACTIVATION_FAILPOINT`): one injected `Hooks` trait or `fail`
crate failpoints, no-op in production. Start with the two biggest files. (M–L)

### 4.3 hiqlite is a private fork now — decide it

`Cargo.toml:149-150` builds `vendor/hiqlite` + `vendor/hiqlite-wal`;
`PLURX-PATCH.md` lists 15 + 3 patches; 66 commits / 51 `fix:` and +78k lines
into `vendor/` this month; upstream 0.14.0 is current and no PR links exist.
ARCHITECTURE §9 still says the mitigation is "openraft fallback is the same
shape" — it is not, any more. Also, one unconditional feature edge in the fork
(`cryptr features=["s3"]`) pulls `s3-simple`, a second `aws-lc-sys`, `quinn`
and a second `reqwest` into every build of plurxd for a backup feature that is
disabled (`cargo tree -d`: `aws-lc-sys 0.39.1 + 0.43.0`, `reqwest 0.12 + 0.13`,
`rand ×3`, `thiserror ×2`). Decide: upstream the generic durability patches
and pin a release, or own it openly (`plurx-raft`, its tests in the fast lane,
the "fallback" sentence deleted). Either way `cryptr = { default-features =
false }`, delete `vendor/s3-simple`, `deny.toml [bans]` on `aws-lc-sys`.
Same shape for semantic search: candle + gemm ×7 + rayon + Oniguruma C build
compiled into every `cargo check` for a 423-line optional feature
(`plurxd/Cargo.toml:26-30`) — `tokenizers` → `fancy-regex` backend and a
`semantic-search` feature, or a spawned `plurx-embed` process per the
isolation design.

### 4.4 What the process is costing

Of 3,859 commits, **57 % changed no product code**; 1,240 (32 %) touched
`validation/regressions.d/`, 502 of them are 7-line receipt files mapping one
earlier fix SHA to a coverage point, keyed by short SHA so every rebase
generates a re-mapping commit (`ca5ca8bd` renamed eight). `validation/
history.py` classifies a commit as corrective by subject regex (`keep`,
`bound`, `remove`, `preserve`, `truth` all match), so prose commits need
receipts too; self-referential receipts exist (`114bad7d` "mark the preceding
ledger commit non-runtime"). `tests/operations` had 136 fix commits, mostly
string asserts on the Makefile/Dockerfile/workflows (`test_contracts.py:1067`
asserts `dockerfile.count("&& apt-get clean") == 2`). The ledger did not
catch any of the six fleet incidents this month; each was found by a human
reading a log, a render or the code.

Proposal: keep `points.toml` coverage points and the fixture gates; retire
per-commit SHA receipts in favour of a PR-level "regression test:
`<path>::<name>`" field checked once at merge; corrective = `fix(`/`perf(`
prefix only; delete text-contract tests that duplicate a runtime assertion
(keep the ~10 security-posture ones); a markdown-link checker instead of
`test_docs_index`/`test_status_pr_claims`; move status prose out of
STATUS.md (2,581 lines) into per-effort docs with a one-line index row.
Target: `tests/` Python under 8k lines, and an effort's exit criterion is an
observed-in-production counter (§5.3). (M)

### 4.5 Release discipline stopped

Last tag v0.3.0 (2026-08-31); `Cargo.toml` still 0.3.0; `[Unreleased]` is
340 lines for ~3,100 commits; the fleet runs `v0.3.0-700-g…`, so "Rolling back
a deploy" (OPERATIONS §341) has no tagged artefact and the fragment-index
outage, the HDR-x264 regression and four schema hotfixes all fall inside one
unreleased span. Meanwhile mobile counters were bumped 92 times (Apple 82 →
173) because `validation/mobile_versions.py` enforces it — the discipline is
achievable; nothing equivalent exists for the server. Tag `v0.3.x` on every
fleet deploy (the playbook can refuse an untagged SHA) and roll `[Unreleased]`
in the tag script. (S)

### 4.6 Ops hardening that is missing

`deploy/plurxd.service` has `NoNewPrivileges`, `ProtectSystem=strict`,
`ProtectHome` and nothing else: no `LimitNOFILE` (systemd soft default 1024
and tokio does not raise it — each session holds pipes, the source fd, up to
seven sidecars, segments and sockets; the hiqlite patch log already records an
fd-exhaustion incident), no `OOMScoreAdjust` (the OOM killer may pick the
daemon over a wedged x265 child), no `TasksMax`/`MemoryHigh`/`PrivateTmp`;
Compose has no `ulimits`/`pids_limit`. Children run at the daemon's priority
(`grep setpriority|ionice|oom_score_adj` in non-test src → none). Fix:
`LimitNOFILE=65536 OOMScoreAdjust=-500 TasksMax=4096 PrivateTmp
ProtectKernelTunables RestrictSUIDSGID LockPersonality`; child `pre_exec`
`setpriority(10)`, `ioprio BE/7`, `oom_score_adj=+500`; raise soft NOFILE at
startup. Also: CI unit tests run against ffmpeg 6 while the shipped image
asserts jellyfin-ffmpeg 8 capabilities (`ci.yml:267`; the action's own
comment: "CI never caught it because CI has never run ffmpeg 8"); the
`Dockerfile` base `rust:1-bookworm` floats while `rust-toolchain.toml` pins
1.97.1; release profile is `lto="thin"` with no `debug="line-tables-only"`
(panics are unsymbolicated) — `panic=unwind` itself is correct for the
isolation design, keep it. (S each)

### 4.7 ARCHITECTURE.md describes a system that no longer exists

Every reviewer was told to read §1–§3 to learn what is deliberate; four of the
numbers there are wrong and one shipped feature is listed as a refusal:
"No DVR, and no scheduler" (§8) vs DVR merged at `aba14096` with a 15 s
scheduler; "watchdog 8 s" vs `ACTOR_HARDWARE_STARTUP_BUDGET = 12 s` and #618
"watchdog removal"; "segment 4 s" vs `SEGMENT_SECONDS = 2`,
`COPY_SEGMENT_SECONDS = 6`, live 1 s; "1-voter raft single node" vs the
unreplicated SQLite mode; "single-file SPA" vs 62 files; "hiqlite (spike) →
openraft fallback" vs the fork; "no roles" vs learners; "six-segment live
window" vs 24; "Replicated-ephemeral Raft KV" vs nothing. `docs/README.md`
marks 44 docs "open" whose own header says merged, and 51 files under `docs/`
are not indexed. One PR: rewrite §2.1/§3/§6/§8/§9 with named constants and
file links (as `transcode/mod.rs:68` already demands: "Nothing may hardcode
the number") and extend `validation/doc_versions.py` to grep the constants
the doc quotes. (S)

---

## 5. Sequencing — what I would do, in order

### 5.1 This week (all S, all verified; ~12 PRs)

1. `output_job_owned` pipes both streams + echo test (§2.1).
2. Fast Rust gate runs `make unit` + workspace clippy; scheduled `ci.yml` on
   `main` every 4 h (§2.2).
3. `ReaderStream::with_capacity(256 KiB)` in the four body sites; pump acks
   per byte budget (§2.4).
4. Listener timeouts + `TimeoutLayer` on JSON groups; gzip `WEB_ASSETS`;
   `hls.min.js` into `WEB_ASSETS`; sidecars hashed; security headers (§2.5,
   W2, W5, W6-now).
5. Font-closure attestation once per capture, timer re-check (§2.7).
6. Encoded-VOD hold = SIGSTOP + low-water mark, cases added to `prodsched`
   tests first (§2.6).
7. Watched-outbox: local read-before-write, skip when unconfigured, backoff
   (S3); TMDB/AniList timeouts (C5); `bounded_replica_reads=true` default
   (S1 part 1).
8. tvOS display criteria (§2.8); audio-session interruptions (§2.10);
   Android `largeHeap` + budget floor (§2.9 part 1); exclude `datastore/`
   from backup (D5 part 2).
9. Web: `enableWorker` on, `recoverMediaError` ladder, global error handler
   (Q10, W7).
10. systemd unit limits + child priorities (§4.6); tag `v0.3.1` and deploy
    (§4.5).
11. Live TV: guide view for the DVR scheduler (L1); one `PeerTransport` (L3).
12. ARCHITECTURE.md rewrite (§4.7).

### 5.2 This month (M)

Encoder defaults as a set with a before/after VMAF on three fixtures — CRF
default, B-frames back, tone-map peak/primaries/dither, deinterlace, 5.1/EAC3
audio, honest master BANDWIDTH/CODECS (§3.1 Q1–Q7). Cluster backup/restore
(§2.3) and snapshot off the writer with a measured build time (S2). Auth
cache + remaining catalogue reads local (S1 part 2). SQLite read pool + index
pass + `ANALYZE` (S6, S7). Android frame-rate matching + background model +
player builder (§2.9, D2, D4). Web local seek within the buffered window (W3)
and `tsc --checkJs` in CI (W8). Apple `Attempt` struct + one item observer
(A3, A4). Live TV shared transport per channel (L4) and start without the
fixed 3 s prefix (L6). Process: receipts → PR field, text-contract pruning
(§4.4). hiqlite dependency cleanup (§4.3 second half).

### 5.3 This quarter (L)

The `transcode.rs` / `hls.rs` / `vodserve.rs` decomposition and registry
unification (§4.1), then `playback_control.rs` and `membership.rs` as pure
transition functions, then the test-seam removal (§4.2). The hiqlite fork
decision (§4.3). The "Replicated-ephemeral" tier or its deletion from the
architecture (S8). And one rule for every future effort, which is the thing
that would have caught five of this month's six fleet incidents: **the exit
criterion is a counter read off `/metrics` on the fleet** —
`prepared_handoff_taken_total` vs `quality_switch_fallback_total`,
`live_tv_starts_succeeded_total`, `fragment_index_artifacts_built_total`
rising while `jobs_dead_total` is flat, `tonemap_graph{chain}` per node — not
a green unit run and a receipt.

---

## 6. Already good — do not "fix" these

- Zero `unwrap()` in ~315k production lines (`unwrap_used` + `-D warnings`);
  exact toolchain and SHA-pinned actions; rustls-only, no openssl.
- Store discipline: rusqlite strictly on `spawn_blocking`, WAL +
  `synchronous=NORMAL` + `busy_timeout` + FK on, STRICT tables, append-only
  migrations with per-migration FK check and newer-schema refusal; the
  progress coalescer and its physical-commit budget test; lease renewals
  batched into one raft txn per node per tick; `json_each(?)` for ID lists;
  `FILE_COLS` excluding `probe_json`; `TimedClient` as the sole hiqlite path
  with the bypass-forbidding test.
- No `std::sync` guard held across `.await` anywhere in the server core;
  every per-session/per-user map TTL-pruned or capped; auth extractors
  applied uniformly (all 205 route registrations checked — no admin route
  falls through); timing-uniform login with a real dummy hash; `openat` +
  `O_NOFOLLOW` file serving; overflow-safe range parsing; browse endpoints
  paginated and batched.
- Process supervision: `kill_on_drop`, owned stderr readers with a bounded
  ring, biased reap-before-signal select, Windows job objects, SIGSTOP pacing
  through the supervisor, `statvfs` emergency margin, working-set hysteresis.
- Copy/remux path: `hvc1`/`dvh1` tagging, `dovi_rpu=strip`, `filter_units`,
  `-map_chapters -1`, `-noaccurate_seek` + `avoid_negative_ts make_zero`;
  `OutputGrade` typing that makes PQ-at-8-bit unspellable; forced-IDR per
  family; `-maxrate 1.5×/-bufsize 2×` with measured rationale; 6-ch AAC
  forced to `channel_layout 5.1`; the VOD frame-grid design and
  `establish_or_verify`; fragment index keyed by the executable closure
  digest.
- HLS authoring for AVFoundation: VIDEO-RANGE/CODECS/SUPPLEMENTAL-CODECS only
  when needed, HDR10 SEI promoted into `hvcC`/`mdcv`/`clli`, forced renditions
  `DEFAULT=NO`, no INDEPENDENT-SEGMENTS on open-GOP copies.
- Live TV: ownership as configuration, signed drains, atomic playlist +
  inventory publication, the 1 s uniform cadence, durable guide cache, the
  pure delivery planner, separate scratch namespace, `env_clear` + GPU
  allow-list.
- Clients: the framework-free policy layers tested against shared fixtures;
  Apple image pipeline (ImageIO downsample, bounded caches,
  stale-while-revalidate, dedup), capability snapshot discipline, Keychain
  tokens, the compatibility ladder ordering; Android `SurfaceView`, tunneling
  on TV, `setEnableDecoderFallback`, route-aware audio, honest DV claims, R8
  on; web content-hashed `immutable` rows with a hard 404 for unknown assets,
  the three ordering gates, `useNativeHls()` keyed on the WebKit AirPlay API,
  MediaCapabilities HEVC tiering with PQ, `esc()` discipline, generation-guarded
  rendering.
- Process: RCA-grade commit bodies (`120a3d29`, `cfba5ff6`, `6febf93e`,
  `57e03a6b`, `93beae93`, `e314a5bd`); `#[ignore]` always with a reason; tests
  added not deleted; the byte-identical web-shell split with its gate;
  `PLURX-PATCH.md` for every vendored patch; mobile version enforcement.

---

## 7. Decisions that are yours

1. **Fast lane scope.** Put `make unit` back in the merge gate (2–4 min), or
   keep compile-only and rely on a scheduled run? I recommend both; the
   schedule alone leaves a window where a red merge deploys.
2. **hiqlite**: upstream and pin, or own the fork openly? (§4.3)
3. **Encoder defaults**: switch to CRF/QVBR + B-frames as one measured change
   with VMAF evidence, or one flag at a time behind the Developer tab? I
   recommend one PR with the three fixtures' numbers in the body.
4. **The ledger**: retire per-commit receipts for a PR-level field? (§4.4)
5. **Semantic search and Windows**: keep in the default build (they cost every
   `cargo check` and the most expensive fast-lane job), or feature-gate /
   drop? Open question 6 in the build appendix asks whether Windows is a
   wanted target.
6. **DVR vs §8**: the architecture's non-goal was reversed on 2026-09-13 —
   record the retention/conflict decisions that reversal implies.

## 8. Open questions only the fleet can answer (prompts for GPT)

- Apple TV in "4K SDR + Match Dynamic Range + Match Frame Rate": does a
  24p HDR10 title switch the display mode? (Decides §2.8's user impact.)
- iPhone: take a phone call mid-film; unplug headphones mid-film. (§2.10.)
- `journalctl -u plurxd | grep -c "spawned a producer generation"` over one
  two-hour encoded session on media1. (§2.6's rate.)
- `plurx_raft_snapshot_seconds{operation="build"}` p99 and state-machine DB
  size on each voter. (S2's severity.)
- `plurx_store_operation_seconds{class="authority_read"}` p50/p99 on the
  three-node lab vs single node. (S1.)
- Size of `guide.json` on media1 today; >2 MiB means L1 is live now.
- `ffmpeg -hwaccel qsv … -vf hwdownload,format=p010,showinfo` on an HDR10
  fixture: are MDCV/CLL and `color_trc` still present after download? (Q3.)
- `memoryClass` / `largeMemoryClass` on the Lenovo, Google TV and Shield.
  (§2.9.)
- Does any node run NVENC or VideoToolbox in production? (Q6.)
- Any DV Profile 5 titles in the libraries? If yes, client logs since 09-14
  show `unsupported_build_error` for them. (§2.1.)

---

*Companion: `ARCHITECTURE-REVIEW-2026-09-20-APPENDIX.md` — the nine full area
reports with every finding, quoted evidence, per-area "already good" lists,
fix-density tables and open questions.*
