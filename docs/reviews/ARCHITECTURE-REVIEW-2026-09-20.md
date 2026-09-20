# Architecture review — where plurx stands after the month, and what to change

**Status:** revision 3 — Fable's revision 2 consolidated with Astra's review ·
**Reviewed:** `main` @ `a1414368` (2026-09-20 04:24 UTC) · **Written:**
2026-09-20 · **Revised:** 2026-09-20 (rev 2 after the adversarial
assessment; rev 3 merging Astra's independent review) · **Scope:** server, store/cluster, streaming pipeline, Live TV,
web/Apple/Android clients, build/CI/ops, and the last month's history
(≈3,900 non-merge commits since 2026-08-20 — 3,860 or 3,906 depending on
whether the window boundary is local midnight or UTC; history back to
2026-08-18 was read)

Nine focused reviews ran in parallel (one per area plus one that read only the
git history), each anchored to `file:line`. I then re-verified every P0/P1 and
every "do this first" item below against the tree myself before writing it
down; where a claim rests on an agent's read alone it says LIKELY, not
CONFIRMED. The nine full reports (128 findings with quoted code, plus each
area's "already good" list and open questions) are in the companion
appendix; this document is the consolidated verdict, ranked.

**Revision 2** folded in an adversarial assessment of the whole review
(`ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md`, 210 dispositions).
**Revision 3** merges Astra's independent review, which was written against
the first draft and adds ten findings this review had not made (Q12,
C12–C16, L10, §3.8, §4.8, §4.9) plus a reproduction of the interlace defect
and two stale tests it found red on `main`; every Astra addition was
re-verified in the tree and, where it was an experiment, re-run here. Where
Astra's copy still carried a first-draft remedy the assessment withdrew, the
revision-2 remedy stands and §0 says so. The appendix is unrevised raw
material; where it and this document disagree, this document wins.

How to read the ranking: **P0** = data loss / outage / security · **P1** =
user-visible stall, quality regression, or resource leak · **P2** = real
performance or maintainability win · **P3** = polish. Size: **S** < 1 day ·
**M** days · **L** weeks.

---

## 0. Revision 2 — what the adversarial assessment changed

The assessment re-read every finding against the tree, ran the real CI
selector, reproduced one proposed query against the actual schema, and
checked platform APIs against SDK headers. Its verdict — "useful defect
inventory, unsafe as a direct implementation handoff" — was right about the
first draft: several remedies removed the condition that made the existing
code safe. Every disposition below was re-checked in the tree before it was
accepted.

**Remedies withdrawn or rewritten (the ones that would have broken something):**

- **Q2 / F-stream-2, B-frames.** `vodgen.rs:399` refuses any sample with a
  nonzero composition offset as part of the frame-grid landing check, and
  VOD-ENCODING.md documents the no-reorder choice. `-movflags
  +negative_cts_offsets` does not satisfy that invariant. B-frames need a
  presentation/decode timeline change proven across random access, init
  identity, leading pictures and restart splices — an M–L design item, not a
  flag. The efficiency figures were unmeasured on this pipeline and are
  withdrawn.
- **C7 / S1, auth caching and logout.** The cache-only admin proof
  (`extract.rs:56`, five minutes) exists precisely so a revoked credential
  cannot be honoured from a stale cache; the two-phase logout is the fence
  that makes that safe. "Plain `DELETE` + best-effort notify" and "extend the
  cache to ordinary auth" both weaken acknowledged revocation. Withdrawn. The
  contention finding stands; fix it with bounded admission (a `lock_owned`
  with timeout instead of `try_lock`), not by dropping the fence.
- **§2.6, encoded-VOD hold.** `Encoding::is_waiting` (`vodencode.rs:160`)
  describes that encoder's own wait; the global signal is
  `Admissions::live_is_waiting` (`admission.rs:580`), and a producer already
  in `Producer::Stopped` yields `Step::Nothing` (`prodexec.rs:183`), so a
  SIGSTOP'd encoder would never be released for a viewer who arrives later.
  The remedy now requires an explicit stopped→release transition driven by
  the shared pool and a test for the late-arrival case. The "1,400 launches
  per film" figure is a model, not an observation; the 7.6 s restart cost is
  one benchmark near the end of a two-hour source, not a constant.
- **§2.7, font attestation.** `EncodedEngine::is_current` re-enumerates fonts
  to catch *additions*, which change libass resolution under an existing
  immutable recipe. A 60 s TTL would let a font install change the bytes a
  recipe emits. The remedy is now: freeze the font environment at recipe
  capture (fontconfig `FONTCONFIG_FILE` pointing at a captured config, or a
  captured font-dir digest verified once per launch), and move the blocking
  `stat` work off the runtime. The per-segment cost finding stands.
- **S9 / F-sc-10, clock skew.** Heartbeats are 10 s apart, so `last_seen_at`
  age cannot detect a 2 s offset; and "timestamp leases with the leader clock
  in the state machine" was read — reasonably — as SQL `unixepoch()` inside a
  replicated statement, which `replicated.rs:1-20` forbids because every
  voter replays it. Both withdrawn. A skew guard needs a timestamp exchange
  with an uncertainty bound (or a trusted time service), and any
  leader-assigned time must be bound as a parameter once, before replication.
- **S2, off-writer snapshot.** The hiqlite writer persists last-applied and
  membership immediately before the `VACUUM INTO` while apply is serialized;
  moving the copy to a read connection must keep the database image,
  last-applied id and membership at one logical cut or restores replay or
  drop writes. The finding stands; the remedy now names that boundary as the
  design requirement. Also: a `probe_json` side table in the same database
  does not shrink the snapshot — only compression or a separate file does.
- **F-sc-8, search.** The proposed `NOT EXISTS (… media_classifications …)`
  is wrong. `classification.rs:24-32` deletes the `classification_fts` row
  when a title or other source fact changes and keeps the
  `media_classifications` row for regeneration, so the current predicate
  finds the renamed title through `items_fts` and the proposal hides it — the
  assessor reproduced this in SQLite. Replace with a rowid point lookup on
  current FTS membership, tested with a renamed title on both backends.
- **A6, Apple polling.** `startStatusPolling` feeds
  `observeDeliveryStarvation` and prepared-switch sampling, not only the stats
  panel; backing it off to 10 s when the panel is closed delays a recovery
  path built around 2 s observations. Separate telemetry from recovery
  evidence first. The item-ready poll replaced a KVO-only wait that could
  hang; any KVO replacement keeps its deadline.
- **F-stream-11 / F-ltv-12, scratch scans.** The directory walk accounts for
  *unpublished* bytes (a temp segment grows before the playlist changes) and
  Live TV's inventory enforces size and regular-file limits. "Segment-index
  deltas only" and "skip until playlist mtime changes" drop those checks. Keep
  the accounting; reduce dispatch cost with one bounded blocking scan.
- **F-stream-9 / F-stream-16, probe skip and fence sampling.** The decode-fact
  cache is keyed by source identity + ffprobe build + catalogue digest +
  stream; a persisted cache keyed by inode/size/mtime drops three keys and
  ignores inode reuse. `SourceFence` proves continuity since *this* open, not
  the scan's identity. Define the persisted attestation first. And do not
  rate-limit the final source-fence check across immutable publication.
- **F-stream-12, frame-rate parser.** `hls.rs:10141` `video_frame_rate` is
  `#[cfg(test)]`; the "cover-art FRAME-RATE" production bug does not exist.
  Withdrawn. The progress-line drift stands.
- **F-ltv-13, `-hls_start_time_offset`.** Not an hlsenc option in FFmpeg 8 or
  9. Withdrawn. `program_date_time` is the muxer's wall clock, not broadcast
  time; the latency measurement needs a defined origin.
- **F-build-11, release profile.** `strip = "debuginfo"` removes the line
  tables `debug = "line-tables-only"` adds; the snippet contradicted itself.
  Corrected below. Fat LTO / CGU=1 / overflow checks are measured trade-offs,
  not free wins.
- **§2.8 / §2.9, platform APIs.** Media3 has no
  `VIDEO_CHANGE_FRAME_RATE_STRATEGY_ALWAYS` (that constant is the
  `Surface` API); the Android remedy is `Display.Mode` selection via
  `preferredDisplayModeId` as jellyfin-androidtv does. For tvOS, apply
  `asset.preferredDisplayCriteria` through the active window's
  `avDisplayManager`; whether the explicit
  `AVDisplayCriteria(refreshRate:videoDynamicRange:)` initializer is in the
  installed SDK is to be checked on `maca`, and nothing depends on it.
  `largeHeap` is a request, not a guarantee — size against
  `largeMemoryClass` after measuring concurrent players and image cache.

**Claims narrowed (the finding stands, the wording was too broad):**

- §2.2: the compile-only fast lane is a **ruling you made on 2026-09-10**
  (DEVELOPMENT_PIPELINE.md, "Decider: Paul"), not drift. What is still true:
  no Rust test runs anywhere on any trigger except a `v*` tag or a manual
  dispatch, and the "batch process fixes full-suite failures later" has no
  input. The gap is a policy question for you (§7.1), not a P0 defect. And
  `cargo test -p plurxd --bin plurxd` is not `make unit`; the proposal now
  names `make unit` (2–4 min warm).
- §1: "every audio transcode is stereo" → every **full video transcode**;
  copy-video audio conversion keeps 5.1 at 320 k and encoded VOD sets 48 kHz.
  "Every authenticated request is a leader round trip" → every authenticated
  request **on the hiqlite backend** (standalone SQLite, capability-authorised
  media and the cache-only recovery routes excepted); parallel poster fetches
  are not serial page latency. "None of these was a decision" → several were
  (no-reorder VOD, release-on-hold, qualified rate-control fallback, bounded
  Android buffers, original backdrops, web seek policy); the review must carry
  their constraints, which §3 now does. "All incidents were found by humans"
  → the six named incidents were.
- §4.2: 681 non-module `#[cfg(test)]` sites include imports, helpers and
  test-only types, not 681 mutated shipping structs. The barriers and pause
  fields in `transcode.rs` are real; the count needs a syntax-aware census.
- §4.5: rollback artefacts exist — OPERATIONS.md §308 documents immutable
  `sha-<12hex>` image tags with ten retained. What is missing is semantic
  tags, a CHANGELOG rollover and a release ↔ fleet mapping. A tag per deploy
  would also trigger the full tag CI; that trade-off is yours (§7).
- §4.7: the 51 "unindexed" docs are in folders the index test exempts
  (`apple-builds/`, `archive/retro-*`, `evidence/`). Withdrawn. The
  ARCHITECTURE.md prose drift stands in full.
- S10: running `validation/ci_scope.py` (not counting glob text) finds
  **3 of 16** `hiqlite_*.rs` and **7 of 24** `sqlite/*.rs` outside
  `cluster_auth` scope, not 7 and 20. Same conclusion, correct numbers.
- W3: direct play and immutable VOD already seek locally; the gap is rolling
  HLS and progressive remux. The heading said "every non-VOD seek".
- W9: `library-grids.js` mounts the shell once and redraws the item region;
  "repaints the whole page" was wrong; the per-batch region rebuild stands.
- F-hist-8 / §4.4: the weekly counts sum to 3,905 and the product/non-product
  partition to 3,849 against a 3,860 headline; receipt subclasses sum to 630.
  The assessor's reproducible query gives 1,037 commits touching
  `regressions.d/` and 134 `fix`-prefix commits touching operations tests,
  against my 1,240 / 136 under a broader "corrective" rule. The order of
  magnitude and the conclusion stand; the exact percentages are now given
  with their rule.
- The 161k-vs-315k production-line discrepancy between two appendix reports
  is two different classifiers (cut-at-first-`cfg(test)` vs
  non-test-file). Both are stated with their definition; neither is "the"
  number.

**Pushed back on, with reasons:**

- The assessment treats "the compile-only gate was a decision" as closing the
  matter. It does not: the same decision record says the batch process picks
  up full-suite failures, and nothing produces them. That is the finding.
- "Keep" verdicts on the ten do-first items are otherwise unchanged; the
  assessment confirmed the code facts for each.

**Added from Astra's review (revision 3), each re-checked in the tree:**

- **C12** — the ordinary scan probe (`scan/probe.rs:190-215`) is a bare
  `Command::output().await` with no deadline, output bound or `kill_on_drop`;
  a hung ffprobe stalls a library scan and dropping the future does not kill
  the child. Confirmed. P1.
- **C13** — every decode-fact lookup takes one global semaphore and hashes
  three executable-sized inputs before checking its cache, and
  `resolve_held_movie_plan` falls back to catalogue facts on *any* error
  including identity changes. Confirmed mechanism; latency unmeasured. P2.
- **C14** — item-detail badges call `fragment_index(...).is_some()`, which
  unpacks every row of the index per video identity, and the same handler
  `stat`s every media path sequentially with no deadline. Confirmed. P2.
- **C15** — telemetry spawns a task per event, reads settings (a consistent
  read on hiqlite) inside it, then does an individual insert; no bounded
  queue. Confirmed. P2.
- **C16** — retired rolling sessions keep their 2 GiB + 64 MiB scratch
  reservation; already owned by the seek-scratch RCA/implementation plan
  (untracked in the checkout as of this writing). Tracking item only.
- **L10** — DVR fan-out writes sinks sequentially with the sink lock outside
  the 30 s write timeout, so one slow destination stalls the shared tuner and
  sibling recordings. Confirmed. P2.
- **Q12** — `Encoder::video_codec_for` gives HDR10 HEVC output only to
  software and QSV; NVENC/VAAPI/VideoToolbox have no HDR10 path. A
  qualification boundary, not a bug; a P2 to widen with measurement.
- **§3.8** — the browser has an evidence-aware adaptive controller; the
  native clients have none, and the legacy stall-ticket plumbing is dormant
  (Apple's `stallReopenIntent(wedge:)` has no call site). Confirmed. P2, L.
- **§4.8** — `node tests/playback/web-policy.test.js` is red on `main`
  (stale call-count assertion at line 6007); `web-control.test.js` is also
  red under Node 22.22.2 at line 3164. Both reproduced here.
- **§4.9** — an ownership map (contract → admission → producer attempt →
  published object → client attachment) as the frame for the decomposition.
- **Interlace reproduction** — Astra's fixture: the current CPU chain emits
  90/90 combed frames tagged progressive; with `bwdif=send_field`, 176/180
  progressive at 59.94. Re-run here on ffmpeg 6.1.1 with identical counts.
- **Amendments Astra made that stand:** §2.3's backup scope (secrets,
  media-root remapping, DVR schedules, isolated restore first, measured
  RPO/RTO, one-node and majority-loss cases); §2.8's note that
  `LibraryChannels.swift:855,901` uses SwiftUI `VideoPlayer` and must be
  assessed separately, and that a staged successor must not change the
  display before it is visible; Q4's note that the raw catalogue probe JSON
  already retains `field_order`, so no blanket rescan is needed.
- **Astra's copy superseded by revision 2 (do not build from it):** Q2
  B-frames via `negative_cts_offsets`; S1's auth cache; S9 heartbeat-derived
  skew; S7's `NOT EXISTS`; L9's `-hls_start_time_offset`; §2.9's
  `STRATEGY_ALWAYS`; the release profile; §2.6 without the stopped→release
  transition; §2.7's timer TTL. (Astra's own C7 rewrite already agrees with
  the assessment.)

**Net effect on the sequence (§5):** the week-one list loses nothing but
splits three items (buffers vs. acks; listener vs. gzip vs. CSP; font I/O vs.
attestation design) and moves the auth cache, B-frames, snapshot offload and
skew guard from "this month" to "design first". Nothing in §5.1 depends on
a withdrawn remedy.

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

1. **No Rust test runs on any automatic trigger.** By your 2026-09-10 ruling
   (DEVELOPMENT_PIPELINE.md; `3cd127e2`) the only Rust job before a merge is
   `cargo check` plus the vendored-hiqlite clippy; the full sweep (`ci.yml`)
   fires only on a `v*` tag or by hand, the last tag is v0.3.0 from
   2026-08-31 — 2,213 commits ago — and runtime schedules are disabled. The
   ruling assumes a separate batch process picks up full-suite failures;
   nothing produces them. That is a policy question for you (§7.1), not an
   accident, but its consequence is concrete: twenty red tests were carrying
   three live production defects on 09-17 (#356), and a Windows-port refactor
   on 09-13 silently un-piped child stdout — **two of its three regressions
   are still on `main`** (§2.1).
2. **The hot paths are built from defaults that were never sized.** Direct
   play and every HLS segment stream in 4 KiB chunks, one blocking-pool hop
   each; the HTTP listener has no timeouts at all; the encoded-VOD producer is
   SIGKILLed and respawned every control beat in steady state; text-subtitle
   burns spawn `fc-list` + `fc-conflist` per *segment*; the encoder runs
   1-pass ABR with B-frames disabled; every full video transcode downmixes to
   stereo AAC; there is no deinterlacer on the file path; the tone-map chain
   infers peak luminance. Some of these were decisions with recorded reasons
   (no-reorder VOD, release-on-hold, the qualified rate-control fallback) and
   §3 carries those constraints; the rest are defaults nobody revisited once
   the lifecycle machinery around them became the focus.
3. **The cluster pays consensus for things that do not need it, and reads
   almost nothing locally.** 225 `query_consistent` call sites (plan baseline
   85); on the hiqlite backend every authenticated request outside the
   capability-authorised media and cache-only recovery routes is a leader
   round trip over the client WebSocket; `bounded_replica_reads`
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
   Test-only barriers and pause fields are compiled into the shipping
   supervisor structs (`transcode.rs:3290-3293` and siblings), so the
   lifecycle the tests pin is not byte-for-byte the one that ships.

The single most useful reframing: **the month optimised for proof-by-test and
proof-by-ledger, and the six named fleet incidents were each found by a human
reading a log or a render, not by a gate.** Fragment-index worker dead 72 h
with zero write errors (`120a3d29`); M6 prepared handoff "enabled by default"
for a week and unreachable by any client; Live TV shipped twice with a start
path that could not succeed; GPU tone-map "never validated on any node". Each
had a production counter that, with an expected-demand denominator and an
alert owner, would have said so. §5 proposes that each effort's exit criterion
adds an observed-in-production number to — not instead of — its pre-merge
tests.

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
contract every caller assumed) and add a unit test that runs a portable child
(`cargo`-built helper or `sh -c`, not `/bin/echo`, so it also runs on the
Windows lane) and asserts stdout, stderr, a non-zero exit status and
cancellation. The `dovi_proofs` cache is in-memory, so the deploy clears it.
The 09-14 date and fleet impact come from the incident commits, not from
observation here.

### 2.2 No Rust test runs on any automatic trigger (policy decision, S to change)

`.github/workflows/main-fast-lane.yml:111-131` "fast Rust gate" = `make
effort-rust-check` (`Makefile:76-77`: fmt-check + `cargo check --workspace
--all-targets`) + `hiqlite-vendor-clippy`. `ci.yml:6-9` fires on `tags: v*`
and `workflow_dispatch` only; the sole cron in `.github/workflows/` is the
weekly `rust-audit`. `lint.yml:15-16` says "Main pull requests run Clippy in
main-fast-lane.yml" — they do not. The 853-file regression ledger maps 511
corrective commits to a `rust-gate` check that no PR runs. The tracked
pre-commit hook still runs workspace clippy, and AGENTS.md assigns focused
local regressions to the contributor — so the gap is enforcement, not
intent.

This is the state you ruled on 2026-09-10 (DEVELOPMENT_PIPELINE.md, "Decider:
Paul"): compile-only PR lane, no runtime schedules, full CI by tag or
dispatch. The ruling assumes a batch process picks up full-suite failures
later; nothing runs the suite, so that process has no input. Options, for
§7.1: (a) `make unit` (not the two `cargo test` invocations the first draft
named — `make unit` is the workspace fast lane and carries the two explicit
FFmpeg restart checks) plus workspace `clippy -D warnings` in the fast Rust
gate, ~2–4 min warm on top of the `cargo check` already paid — note `cargo
check` does not pay test codegen or linking, so "already compiled" is only
partly true; (b) a scheduled `ci.yml` on `main` every 4 h with job conditions
that keep the heavy cluster lanes out and a concurrency expression that
cancels schedule events; (c) both. The exposure window under (b) alone is a
red merge that deploys before the next run.

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
see §4.3) as the mechanism, but the deliverable is the *procedure*: a
`plurxd backup --output` that captures one consistent cut (§3.2 S2 —
database image, last-applied id and membership together), the credential
encryption key and schema version alongside it; off-node retention (a copy
beside the same three disks does not cover site loss); a restore that
re-creates a single voter with a new cluster identity and fences the old
one; an integrity check on the artefact; a nightly leader-singleton schedule
through the existing job lease; `plurx_backup_last_success_seconds` on
`/metrics`; and a restore drill in `container-smoke`. A raft snapshot is not
this — it is not portable across cluster identities. Astra's scope additions,
adopted: the artefact must cover secrets and the credential key, schema
version, cluster/node identity, media-root remapping, watch and reading
state, DVR schedules and integrations, and say which derived caches are
rebuilt rather than restored; restore to an isolated instance first; measure
RPO and RTO with realistic data; test both the one-node and the majority-loss
case. Do not improvise a force-new-cluster command around copied raft files.

### 2.4 Media bodies stream in 4 KiB chunks through a blocking-pool hop per chunk (P1, S)

`http/stream.rs:2940,2957` and `http/hls.rs:13402,14012` use
`ReaderStream::new` (tokio-util default 4096 B); each read is a
`tokio::fs` `spawn_blocking` round trip, and the HLS pump additionally sends
every chunk through `mpsc::channel(1)` (`hls.rs:9900`) and waits for a
oneshot ack. An 80 Mb/s UHD direct play is ~2,500 blocking-pool hops per
second per viewer; a 4 MB segment is ~1,000 read→channel→hyper handoffs. The
right idiom is already in the tree: `internal_media.rs:116` and
`offline.rs:1192` use `with_capacity(256 * 1024)`. Two separate changes: (1)
the buffer size at the four sites, measured for throughput and concurrent
memory (256 KiB × viewers is the new resident cost); (2) the pump's
per-chunk ack, which also proves downstream acceptance before delivery
accounting and completion — batching it must keep the exact final-byte,
dropped-consumer, deadline and ownership behaviour, so it is a smaller,
later change with its own tests, not part of (1). The hop count is
arithmetic; the end-to-end gain is to be measured, not assumed.

### 2.5 The HTTP listener has no timeouts, limits or compression (P1, S)

`main.rs:2418` `axum::serve(...)` with no `.timer()`, so hyper's own 30 s
header-read default is silently disabled (`hyper-1.10.1
common/time.rs:72-76`); `http/mod.rs:629-643` has only `TraceLayer` and the
capacity gate — no `TimeoutLayer`, `ConcurrencyLimitLayer`, `LoadShed`,
request-id or `CompressionLayer` anywhere in `crates/`. "No limits" is too
broad: per-route body limits, the cluster capacity gate and per-endpoint
publication budgets exist; what is missing is the connection-level layer
(header read, idle keep-alive) and handler deadlines. SECURITY.md defers to a
reverse proxy, but bare LAN is the documented normal deployment. The web app
is 2.6 MB raw / 0.87 MB gzip across 71 script/style requests before `boot()`
runs (`web.rs:207-217` sets no `Content-Encoding`; `flate2` is already a
direct dependency) — execution is serial; the fetches are not necessarily,
so measure a cold and a warm waterfall before crediting a number.

Three separate PRs: (1) `hyper_util::server::conn::auto::Builder` +
`TokioTimer`, `header_read_timeout(15s)`, h2 keepalive, and `TimeoutLayer`
on JSON route groups sized per group (settings updates and scans are
legitimately long) — never on streaming routes; (2) precompress `WEB_ASSETS`
once at startup in a `LazyLock` beside `ASSET_HASHES`, negotiate
`Accept-Encoding` properly (q-values, `gzip;q=0`), send `Vary:
Accept-Encoding` and keep the validators — do not wrap the router in
`CompressionLayer`, it would touch `/hls/*`; (3) the security headers in W6.

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
where restarting costs a reposition"). The launch count is a model of the
steady state, not a fleet observation, and the 7.6 s figure is one benchmark
near the end of a two-hour source including reap — count real generations
per session before sizing the win (§8).

Fix, with the correction from the assessment: SIGSTOP encoded renditions
like copy ones **and** make the stopped producer yield to a later arrival.
`Encoding::is_waiting` (`vodencode.rs:160`) is that encoder's own wait; the
pool-wide signal is `Admissions::live_is_waiting` (`admission.rs:580`), and a
`Producer::Stopped` currently yields `Step::Nothing` (`prodexec.rs:183`), so
without an explicit stopped→release transition a SIGSTOP'd encoder holds its
permit against a second viewer forever. Add that transition (bounded
re-evaluation on the pool signal), add a low-water mark so a producer runs
in ≥90 s bursts, and cover "second viewer arrives after the first has
stopped" in `prodsched`/`prodexec` tests before touching `vodserve`.

### 2.7 Text-subtitle burns spawn `fc-list` and `fc-conflist` per producer launch **and per segment** (P1, S)

`vodserve.rs:7483` calls `recipe_engine_is_current` inside `materialize()`
(per segment) and again at `:6741` per spawn; it reaches
`EncodedEngine::is_current` (`ffmpeg.rs:1612-1621`) which calls
`font_render_engine_inner()` (`:1806-1850`: two process spawns plus a
synchronous `std::fs::metadata` on every font file, on a tokio worker) every
time, nothing memoised. A producer at 3× realtime materialises a segment
every ~0.7 s; the per-segment cost (two spawns + thousands of stats) is real
but unmeasured. Combined with 2.6 this is also paid per respawn.

Fix, corrected: the re-enumeration exists to catch font *additions*, which
change libass resolution under an existing immutable recipe, so a timer TTL
would let a font install silently change the bytes a recipe emits. Two
independent changes instead: (1) move the synchronous `stat` work
(`engine_objects_are_current`, `engine_path_version`) off the runtime into
`spawn_blocking` — no contract change; (2) freeze the recipe's font
environment at capture (a captured `FONTCONFIG_FILE`/font-dir set with its
digest recorded in the recipe, verified once per launch) so per-segment
re-enumeration becomes unnecessary *because the inputs cannot change*, not
because we stopped looking. (2) is the design item; (1) can ship this week.

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
Fix: after `open()` (`PlayerController.swift:4695`), when the active window's
`avDisplayManager.isDisplayCriteriaMatchingEnabled`, set its
`preferredDisplayCriteria = item.asset.preferredDisplayCriteria` (the route
Apple documents for `AVPlayerLayer` apps), guarded against a stale
attachment, and reset to `nil` in `stop()`; same for the Live TV and
library-channel players. The explicit
`AVDisplayCriteria(refreshRate:videoDynamicRange:)` initializer the first
draft named is listed for tvOS 17+ in Apple's reference but the assessor
did not find it in the installed SDK header — check on `maca` before using
it; nothing depends on it. The SDR/60 consequence above is the expected
behaviour given the API contract, not an observation: verify on the bedroom
Apple TV in Apple's default "4K SDR + Match Content" output mode by reading
the TV's actual HDMI mode before, during and after playback — that is the
GPT test (§8). Two further points from Astra: `LibraryChannels.swift:855,901`
uses SwiftUI `VideoPlayer`, whose framework-managed display behaviour is
different and must be assessed on its own; and criteria must bind to the
*committed visible* asset — a staged prepared successor must not change the
display mode before it becomes visible. Verify 23.976/24/25/50/59.94 and
SDR/HDR/DV across replacement and exit, not just first play.

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
primed. Its own instrumented test proves a full byte target can start
playback before the time threshold, so a small buffer is a resilience
constraint, not by itself an 8 s stall — the stall exposure is LIKELY on
high-bitrate remuxes over Wi-Fi, to be measured. Fix, corrected: Media3 has
no `VIDEO_CHANGE_FRAME_RATE_STRATEGY_ALWAYS` (the `ALWAYS` constant is the
`Surface.setFrameRate` API); do what jellyfin-androidtv does — select a
`Display.Mode` via `preferredDisplayModeId` from the plan's source fps before
`prepare()`, wait on `onDisplayChanged` (≤2 s), TV-only, behind a setting,
and apply it to progressive Live TV too. For the buffer: `largeHeap` is a
request, not a guarantee; measure `memoryClass`/`largeMemoryClass` and real
process memory on the Lenovo, Google TV and Shield (§8), then size the
incumbent against `largeMemoryClass` with headroom for the image cache and
a primed successor, and shrink only the successor while priming.

### 2.10 Apple: no `AVAudioSession` interruption or route-change handling; the stall detector "recovers" every system pause (P1, S–M; LIKELY on device)

`PlayerController.swift:2610-2612` sets the category and nothing observes
`interruptionNotification` or `routeChangeNotification` in any of the three
player stacks. The recovery monitor (`:5120-5127`) keys on `wantsPlayback` and
a stationary clock. Traced through the detector's constants, a phone call,
Siri, or pulling the AirPods out pauses AVPlayer, the nudge calls `play()`
after three 2 s samples (film resumes out of the speaker — the exact thing
Apple's route-change guidance exists to prevent), a same-delivery reopen
follows after six, and a one-minute call ends on a terminal "Playback
stopped." That sequence is the code path, not a device observation; whether
the reopen leaves a second live server session is likewise to be checked.
Fix: observe both notifications in all three players, gate `shouldMonitor`
on a `systemPaused` flag that is distinct from user pause and from
background/PiP, resume only on `.shouldResume` with `wantsPlayback` still
set, set `wantsPlayback = false` on `.oldDeviceUnavailable`, pass `mode:
.moviePlayback`, and keep the existing iOS/tvOS audio-session split.
Two-minute experiment
on the iPhone to confirm.

---

## 3. Findings by area

Each area lists what to change in priority order. The appendix has the full
finding text; only the evidence anchor and the decision are repeated here.

### 3.1 Video quality — the encoder is running defaults nobody revisited (all P1, mostly S)

| # | What | Evidence | Change |
|---|---|---|---|
| Q1 | Default rate control is 1-pass ABR on every encoder family; the swept QVBR/CRF mode is opt-in | `encoder.rs:106-110` `RateMode { #[default] Bitrate, Quality }`; `transcode.rs:12552-12562`; `bitrate_for_height` never consults `file.bitrate` | Bitrate is a deliberate qualified-fallback policy, not an oversight — so change it with evidence: compare CRF/QVBR against ABR on representative content per encoder family, make `Quality` the default only for families that pass (x264, and QSV already swept at 22), keep the HDR10 grade's separately qualified policy and the VBV bounds. Do **not** cap `maxrate` at 1.2 × source bitrate universally: a low-bitrate HEVC/AV1 source re-encoded to H.264 legitimately needs more |
| Q2 | Encoded VOD disables B-frames (`-bf 0`) on every encoder — **by design**, and the design is enforced | `vod.rs:270-275`; `vodgen.rs:399` rejects any sample with nonzero CTO in the landing check; VOD-ENCODING.md | A flag change is refused by the validator and is withdrawn (§0). Enabling reorder is a timeline-contract change: composition offsets in the frame grid, init identity, leading pictures at random access, and restart splices all need production decode proofs. Efficiency gain is real in general but unmeasured here. Design item, M–L |
| Q3 | CPU tone-map: no explicit `peak`, primaries converted after the curve, 8-bit output without dither | `mod.rs:1040-1056` | LIKELY, not confirmed: FFmpeg's `tonemap` reads MaxCLL/mastering side data when no `peak=` is given, so the defect only bites when the hardware download strips it — test that first (§8). Then: store MaxCLL/max-luminance at scan and emit `peak=` in `npl` units; `zscale=p=bt709` before `tonemap`; `dither=error_diffusion`; a 1,000-nit default is a policy assumption to be stated as such. Do not blindly substitute `bt2390` across different filters |
| Q4 | No deinterlacer on the file path; the normalised scan fields and the decode-fact probe omit `field_order` (the raw catalogue probe JSON retains it, so no blanket rescan is needed); **reproduced** — see §3.1.1 | only `live_tv.rs:6135` has `bwdif`; `scan/probe.rs:279`; `decode_facts.rs:3343` list lacks `field_order` | Capture `field_order` (it can be `unknown` or wrong — treat `!= progressive` as a hint, verify with `idet` on a sample); `bwdif` before `scale`, choosing frame vs field output deliberately because field output doubles the cadence and changes grid, bitrate and frame-rate metadata; `vpp_qsv=deinterlace=2` / `deinterlace_vaapi` on hardware graphs after qualification |
| Q5 | Every **full video** transcode is stereo AAC 160 k with no downmix matrix; no EAC3/AC3 exists although ARCHITECTURE §3 promises it. Copy-video audio conversion already keeps 5.1 at 320 k and encoded VOD already sets `-ar 48000` (`vod.rs:266`), so the rolling path is the one without a sample-rate floor | `TranscodeOptions::default` `mod.rs:895-913`; `hls_args_inner` `:1661-1666` | Carry the profile's max channels/codec into `TranscodeOptions` and into the recipe identity, muxing and manifests; `eac3 640k` when claimed, else `aac -ac 6 320k`; `-ar 48000` on the rolling path; an explicit stereo downmix matrix chosen for the source channel order and verified for dialogue level and clipping — the appendix's pan string is illustrative, not a drop-in |
| Q6 | NVENC leaves profile/AQ/lookahead to encoder defaults and is never zero-copy (no CUDA graph); VideoToolbox similar | `encoder.rs:436-444` (does not force `main`; leaves it to the driver); no CUDA pipeline in `pipeline.rs` | `-profile:v high -bf 3 -b_ref_mode middle -spatial-aq 1 -temporal-aq 1 -rc-lookahead 20`; `Pipeline::Cuda` behind the boot probe. Only matters if an NVENC node exists — open question |
| Q7 | Master playlist advertises the *source* BANDWIDTH/RESOLUTION for transcodes and omits CODECS on SDR variants | `hls.rs:12413-12461` | RFC 8216 §4.3.4.2 says peak of the variant; AVPlayer filters on it and the Apple panel reads it for its network tone. Emit the resolved output geometry, the exact codec/sample-entry string for the output (not a hard-coded `avc1.640028`), and a peak BANDWIDTH that counts video `maxrate` + audio + container overhead. The recorded "SDR masters without CODECS" hardware ruling must be re-qualified, not overwritten. The Apple-panel consequence is to be reproduced |
| Q8 | Rolling path: no `-sc_threshold 0` (extra scene-cut IDRs; whether the forced 2 s grid actually drifts is unverified); HEVC Main10 in MPEG-TS; `tonemap_opencl=hable` | `mod.rs:1651-1653, 1696-1697`; `pipeline.rs:367` | Three separate small changes, each with its own check (segment starts, client container support, colour output) — not a bundle |
| Q9 | Live TV: encode bitrate uses source fps while `bwdif=send_field` doubles it — 1080i sports get half the bits; captions dropped on VAAPI, no `CLOSED-CAPTIONS` rendition | `live_tv.rs:6110-6135`; `:5992` `-sn -dn` | Compute output fps in the planner with rational arithmetic and keep the bandwidth caps. Captions: `-sn -dn` does not itself prove A/53 SEI is dropped — audit preservation through each decode/filter/encode path with a real captioned fixture, add `-sei +a53_cc` on VAAPI if it is missing, keep the documented VideoToolbox `-a53cc 0` workaround until re-proven, and only then advertise `CLOSED-CAPTIONS` with correct service ids — a phantom track is worse than none |
| Q10 | Web: `enableWorker:false` (main-thread TS demux); a single fatal media error skips hls.js's recovery ladder and permanently downgrades to transcode; capability probe asserts H.264 with no ceiling | `player.js:560`, `prepared-replacement.js:275`; `player.js:756-795` (no `recoverMediaError` call anywhere) | Delete the two lines after exercising worker start/fallback in the vendored build with the custom loader (the build already falls back inline on worker failure). Recovery: one `recoverMediaError()` per attach for media fatals that are not incompatible-codec errors, **inside** the existing attempt fences and shared retry budget — the fatal path currently reports failure before rescue, so recovery must move ahead of that report, not after it; `swapAudioCodec()` only for audio-append faults |
| Q11 | Apple claims no `flac`/PCM so lossless audio is re-encoded; Android under-claims Vorbis, probes 30 fps only, never states HEVC Main10 | `Caps.swift:142`; `CapsPolicy.kt:260-284`, `Caps.kt:157` | Prove each container/transport/profile tuple on a device before claiming it; a fixture asserting the new string is not decoder evidence. Drop the DTS-HD-on-tvOS speculation from the Apple list until a receiver chain proves it |
| Q12 (Astra) | Output codec is coupled to dynamic range: `video_codec_for` gives HDR10 HEVC Main10 only to software and QSV; NVENC, VAAPI and VideoToolbox have no HDR10 output path. A deliberate qualification boundary (the comment records the measurement), not a defect | `encoder.rs:239-252` | P2: qualify one more fleet-relevant codec/GPU graph end to end — decode, filters, subtitle composition, metadata, delivery, cache identity — with the existing `scripts/bench` corpus plus an HDR reference evaluation (VMAF alone cannot certify highlights or DV conversion). Keep H.264 as the compatibility choice; AV1 later, fleet-driven |

The clients' *policy* layers and the copy/remux path are in good shape (see
§6). The gaps are in the encode arguments, the master playlist, and — per
Astra — the media facts the contract is resolved from; argv edits alone do
not complete Q4, Q5 or Q12.

#### 3.1.1 Q4 — the interlace defect, reproduced

Live TV deinterlaces with `bwdif` (`live_tv.rs:6135`); finite playback's
filter construction (`transcode/mod.rs:981`) scales and colour-processes with
no deinterlace decision; the scanner's normalised video fields
(`scan/probe.rs:279`) omit field order although the raw probe JSON keeps it;
the decode-fact model has no interlace policy. DVR records broadcast MPEG-TS
unchanged, so the same programme takes different paths live and saved.

Astra's experiment, re-run here on ffmpeg 6.1.1 with identical results: a
3 s top-field-first MPEG-2 fixture at 29.97 through the current CPU SDR
`scale,format` + x264 chain produces H.264 *tagged progressive* that `idet`
classifies as 90 TFF / 0 progressive; the same source with
`bwdif=mode=send_field:parity=auto:deint=interlaced` first produces 59.94 fps
and 4 TFF / 176 progressive. `idet` is a heuristic, not a quality score, and
this is the filter chain, not the whole HTTP pipeline — but it is a
reproducible fixture. Commands are in §9.

Change: carry field order / scan type into the resolved media contract (the
bound probe's `-show_entries`, its schema, digest and cache identity, and the
selected-stream handling move together); choose frame-rate vs field-rate
output deliberately, because field-rate doubles the cadence and changes the
grid, bitrate and frame-rate metadata (Q9's Live TV bitrate bug is this
mistake already shipped); never deinterlace progressive material or treat
telecine as interlace. Acceptance: 480i/576i/1080i fixtures, moving sport and
text, progressive controls, mis-flagged and telecined sources, inspected on
output frames, on the shipped ffmpeg and each enabled GPU graph.

#### 3.1.2 Q5 — resolve audio independently of video

`options_for_tone_map` (`transcode.rs:14576`) leaves audio at its defaults
(`mod.rs:895`: 2 ch / 160 k) and the finite HLS encoder (`mod.rs:1655`)
selects AAC unconditionally, so a resolution change or subtitle burn downmixes
audio that was compatible. Direct and copy/remux paths are separate and not
accused. Change: copy a compatible selected stream; otherwise encode to a
negotiated codec/layout; downmix only when the route requires it or the viewer
asks; capability means container and actual sink, not the client's codec
name; carry the decision through output contracts, ladder totals, playlist
`CODECS`/`CHANNELS`, cache identity, offline packaging and prepared handoffs;
audio-offset correction may still force an encode. Acceptance: a video-only
quality change with AAC 5.1 and with compatible AC-3/E-AC-3, checking
channels, language, A/V sync and receiver output before and after, plus the
stereo/Bluetooth fallback.

#### 3.1.3 Q12 — widen the codec/GPU qualification deliberately

Quality-controlled SDR encoding exists; QSV has a recorded calibration, the
other families' defaults are candidates in the source comments. Make codec,
bit depth, dynamic range and rate control explicit, compatible dimensions of
the output contract; start with one commonly used fleet GPU and HEVC SDR/HDR
where it measures a benefit; publish source, delivered and rendered facts
separately. Acceptance: delivered quality per byte, realtime headroom, GPU/CPU
load and power on the target host across grain, animation, dark gradients,
sport, HDR10, HLG, DV variants and burns.

### 3.2 Store and cluster — consensus where none is needed, nothing local

| # | Sev | What | Evidence | Change |
|---|---|---|---|---|
| S1 | P1 | Default read path is a leader round trip for everything: `user_for_token` per authenticated request (122 of 205 routes), catalogue reads, settings, session routes; 225 `query_consistent` sites vs plan baseline 85; `bounded_replica_reads` default `false` so `CatalogueReader` returns `None` | `hiqlite.rs:3980-3997`; `extract.rs:627-636`; `config.rs:170`; vendored `client/query.rs:11-20` ("very expensive … pauses the raft") | `bounded_replica_reads=true` by default is a **consistency-policy change**: it needs lag/fence/fallback coverage per route before it flips, then route the remaining catalogue reads through `CatalogueReader`. **Do not** copy the cache-only admin proof onto ordinary auth (§0): that proof is deliberately narrow and its five-minute TTL is bounded by the revocation fence. If an auth cache is wanted it needs the full user/role state and the same revocation protocol; the safer first step is simply fewer consistent reads per request. A justification-comment census keeps the count from growing but is not a latency measurement — see §8 |
| S2 | P1 | Snapshot every 10k log entries is a `VACUUM INTO` inline on the state-machine writer; with `probe_json` at 5–30 KB/row the DB is ~10 KB × files, and a ≥8 s stall self-fences every 12 s session lease on every node | `migration.rs:2327` `default_raft_config(10_000)`; vendored `writer.rs:503-535, 763-767`; `media_sessions.rs:89-90,133-134` | Measure `plurx_raft_snapshot_seconds{operation="build"}` and DB size first — the ≥8 s consequence is a model until then. Raise `logs_until_snapshot` from measured build time, disk and restart-replay cost (a blanket 100k–500k also lengthens log replay for a restarting voter). An off-writer build (patch #16) must keep the database image, last-applied id and membership at **one logical cut** or restores replay/drop writes — that boundary is the design requirement. A `probe_json` side table in the same database does not shrink the snapshot; only compression or a separate file does |
| S3 | P2 | Watched-outbox drain is an unconditional 1 Hz `UPDATE … RETURNING` raft proposal on every voter forever — 259k no-op fsynced entries/day driving S2's cadence | `watched.rs:192-199`; `hiqlite_durable.rs:702-722`; `main.rs:2375-2377` | Local read as a *hint* only — keep the atomic replicated claim; skip the tick when the `MONARR_URL` setting is unset but still refresh that setting on a bounded interval; 5–10 s idle backoff; leader-singleton via the job lease with bounded failover. 259k/day is proposals, not necessarily fsyncs. Audit the takeover loop (`media_sessions.rs:4353-4377`: two consistent `get_setting`/2 s per node with the feature disabled) the same way |
| S4 | P1 | No backup/restore for an activated cluster | §2.3 | §2.3 |
| S5 | P2 | A failed snapshot build is fatal to the raft node; storage floor is a fixed 512 MiB regardless of DB size while `VACUUM INTO` needs ≥ DB size | `membership.rs:144`; openraft `raft_core.rs:1381` | Floor sized from DB image + WAL + retained snapshots + concurrent temp files on the real filesystem (1.5 × DB is a starting proposal, not an established bound); defer snapshot admission on low space rather than inventing a retryable error the openraft path does not have; alert on errors and on low-space refusals |
| S6 | P2 | SQLite backend: ~70 read-only methods (incl. `user_for_token` and every Home rail) run on the single writer mutex; `user_for_token`'s conditional UPDATE takes the WAL write lock every call; `READ_CONNS = 2` | `sqlite/users.rs:273-299`; `sqlite/mod.rs:1300` | `with_read` for every closure that is genuinely read-only and does not rely on writer-connection consistency; split `user_for_token` into read + rate-gated write with the token-deletion race covered; size the read pool by measurement (each connection multiplies cache memory) rather than a fixed eight |
| S7 | P2 | Home/Next-Up/library-page queries are O(catalogue) per request; no `ANALYZE`/`PRAGMA optimize` ever; missing `(library_id, kind, sort_title/added_at/year)`, `(kind, parent_id)`, `tmdb_id` indexes; search does `rowid NOT IN (SELECT rowid FROM classification_fts)` — a full FTS scan per query | `media.rs:856-896, 970-1000, 1021`; `watch.rs:488-520` | Indexes chosen from `EXPLAIN QUERY PLAN` on representative data (some proposed composites lead with `kind` where the query filters only `library`); rank `(id, key)` then join after `LIMIT` only if the candidate window widens until the distinct-group result is complete; **not** the `NOT EXISTS` on `media_classifications` (§0 — it hides renamed titles); `PRAGMA optimize` at close, `ANALYZE` after big scans; keyset pagination with proven order equivalence |
| S8 | P2 | The "Replicated-ephemeral / Raft KV with TTL" tier in ARCHITECTURE §2.2 does not exist; hiqlite is built without `cache`; renewals and heartbeats are durable SQLite writes | `Cargo.toml:38` | Correct §2.2 now. Adopting an ephemeral tier later is a separate durability/failure design — lease, owner and epoch rows are not thumbnails, and TTL semantics on them need their own contract |
| S9 | P2 | No runtime guard on cross-node clock skew although lease expiry and takeover are decided on wall clocks; only prose requires ≤250 ms | `media_sessions.rs:3888-3899, 4384-4388` | Withdrawn remedies (§0): 10 s heartbeats cannot measure a 2 s offset, and any clock evaluated inside replicated SQL is replayed per voter. Real remedy: a timestamp exchange on the existing peer RPC with an explicit uncertainty bound (or a trusted time-service reading), a metric, and a takeover/readiness refusal above a bound that accounts for delay asymmetry and unknown state; leader-assigned times bound once as parameters before replication |
| S10 | P2 | Two hand-maintained SQL implementations drift independently; running the real selector (`validation/ci_scope.py`) shows **3 of 16** `hiqlite_*.rs` (classification, DVR, library_channels) and **7 of 24** `sqlite/*.rs` (classification, DVR, library, library_channels, outbox, trakt, watch) outside `cluster_auth` scope; the `$N`-placeholder census is hiqlite-only although the SQLite twin was the last one to break (`cfba5ff6`) | `validation/ci_scope.py` executed against `points.toml` | Add the ten paths; run a placeholder validator on the `?N` side (SQLite binds by index, so it is a different check, not the same code); long-term one SQL source that generates both placeholder styles with matching parameter order — a mechanical `?N`→`$N` rewrite is unsafe |
| S11 | P3 | SQLite housekeeping at 2006 defaults: only four pragmas, `prepare_cached` used twice, permanent mutex poison, no boot `quick_check`, `files` trailing columns after `probe_json` | `sqlite/mod.rs:1352-1355, 1583-1584` | Measure before tuning: cache/mmap multiply per connection. `journal_size_limit`; `prepare_cached`; on poison, reopen and validate the connection rather than `into_inner` through possibly interrupted state; a bounded boot `quick_check` with an explicit refusal/recovery policy; a probe side table is a row-access win with migration cost, not a snapshot-size win |

### 3.3 Server core

| # | Sev | What | Evidence | Change |
|---|---|---|---|---|
| C1 | P1 | 4 KiB media bodies | §2.4 | §2.4 |
| C2 | P1 | No listener timeouts/limits | §2.5 | §2.5 |
| C3 | P1 | Scanner walks the tree with synchronous `WalkDir` and per-file `std::fs::metadata` on a tokio worker; zero `spawn_blocking` in `scan/` | `scan/mod.rs:494, 358-368` | Walk in a bounded `spawn_blocking` producer in pages of ~256 with a bounded channel (an unbounded queue just moves the problem) and keep order, progress, error reporting and cancellation; `tokio::fs::metadata` for per-file reads. On NFS this pins a runtime worker for the whole scan; the throughput loss is to be measured on the real core count |
| C4 | P1 | Plex façade `section_all` is an N+1 over up to 5,000 items with no `X-Plex-Container-Start/Size` | `plex.rs:191-267` | Batched `files_for_items`/`child_counts` (helpers exist in `browse.rs:236/247`); honour container paging. Only matters if Kodi/PKC is in use |
| C5 | P2 | TMDB/AniList clients have no timeout; enrichment runs under a leased singleton so a hang blocks every node | `tmdb.rs:140-144`, `anilist.rs:59-62` | `connect_timeout(5s)`, response and body deadlines, a bounded retry budget, and a per-item deadline; a timed-out write is not proof it did not happen. The same for `post_join_request`. Whether a hang really blocks every node also depends on lease renewal/cancellation — verify separately |
| C6 | P2 | Image serving SHA-256s the whole file on the runtime per request, 503s local hits above 8 in flight, no `ETag`; backdrops stored at `original` | `images.rs:33, 87-89, 255-263, 838-847`; `metadata/mod.rs:43-44` | Cache a *verified* digest against a strong file identity (size+mtime+inode alone does not prove content-addressed bytes match their name — keep the corruption quarantine); separate local-read from peer-fetch permits but keep local reads bounded; `ETag` = digest; **add** smaller derivatives (`?size=`) for grids — `original` backdrops were a deliberate fix for visible upscaling on TV (`metadata/mod.rs:38`), do not revert it |
| C7 | P2 | Logout/user mutations are a cluster-wide two-phase protocol behind a process-wide `try_lock`; the second concurrent one gets 503 while the client clears its bearer anyway | `auth.rs:100-109`; `extract.rs:221-228`; `internal_auth_revocation.rs:265-354` | Keep the two-phase fence (§0 — it is what makes the cached admin proof safe); fix contention with `lock_owned` under a timeout instead of `try_lock`, so concurrent logouts queue rather than 503. Best-effort revocation is withdrawn |
| C8 | P2 | No login throttle per account/IP; tokens never expire (no `expires_at` on `tokens`); `?token=` accepted on every route | `auth.rs:45-91`; `sqlite/mod.rs:72-78`; `extract.rs:547-577` | Bounded `(username, ip)` backoff map with trusted-proxy handling and a cap so it cannot be used to lock an account out; token idle expiry is a product decision (offline clients, long-lived TV sessions, recovery proofs) — pick the window deliberately and revoke cached proofs consistently; a devices list; narrowing `?token=` needs client migration first |
| C9 | P2 | 1 s route-cache TTL ⇒ ≥1 consistent read/s per active HLS session behind one global `tokio::Mutex<HashMap>` | `media_sessions.rs:107, 1485` | Measure hold/wait time and query counts first; the mutex is not held across the Store call. Any longer positive TTL must be proven against release, replacement and fencing, which change authority immediately rather than at lease expiry; shard the map separately |
| C10 | P3 | No HTTP RED metrics, no request ids, access log at DEBUG, no JSON log format, ANSI escapes into journald, no panic hook (task panics never reach `logbuf`) | `http/mod.rs:629-638`; `main.rs:1542-1560` | `MatchedPath` counter+histogram middleware, `SetRequestIdLayer`, `with_ansi(is_terminal())`, `PLURX_LOG_FORMAT=json`, panic hook → `tracing::error!` |
| C11 | P3 | Direct play carries no `ETag`/`Last-Modified`/`Cache-Control` and treats `If-Range` as a full 200, contrary to ARCHITECTURE §3 | `stream.rs:2828-2832, 2941-2968` | Add strong validators (an id/size/mtime tuple is not strong under in-place replacement — use the held-file identity) and honour `If-Range`; the current full-200 on an unvalidated `If-Range` is conservative, not wrong. A validator match never bypasses authorisation or the source-identity check |
| C12 (Astra) | P1 | The ordinary scan probe is `Command::output().await` with no deadline, no output bound and no `kill_on_drop`; scanning is sequential, so one hung ffprobe stalls the library, and dropping the future on lease loss (`state.rs:4371`) does not kill the child | `scan/probe.rs:190-215` | Share the existing bounded probe primitive (`ffmpeg.rs::bounded_command_output_with_limits`, 5 s / 16 MiB, piped, `kill_on_drop`) with `plurx-core` instead of adding another spawn pattern; establish reap completion before releasing admission; typed transient/permanent failure and continue to the next file. Fix §2.1's helper first. Acceptance: a sleeping probe, an output-flooding probe and a lease loss each leave no child behind — §3.3.1 |
| C13 (Astra) | P2 | Every decode-fact lookup takes one global semaphore (`probe_gate`, permits = 1) and `validate_current` hashes the held executable, the immutable snapshot and the path object in full (up to 512 MiB each) *before* the cache is consulted; the whole request shares a 2 s budget and `resolve_held_movie_plan` falls back to catalogue facts on **any** error, including `ProbeChanged`/`SourceChanged`, at debug level — contention changes fact provenance, not just latency | `decode_facts.rs:26, 464, 2887-2931`; `transcode.rs:14070` | Instrument gate wait, identity validation, source observation, hit and collection separately; amortise validation of the attested immutable image per generation while keeping tamper checks on mutable objects (never mtime-only); classify fallback reasons and stop treating identity changes like timeouts. Measure before optimising — §3.3.2 |
| C14 (Astra) | P2 | Item detail decides an index badge with `fragment_index(...).is_some()`, which loads and unpacks every packed row (≈98 KB for a 4,100-fragment index) per video identity; the same handler `tokio::fs::metadata`s every media path sequentially with no application deadline, so a slow NAS holds up metadata already in the database | `http/browse.rs:394, 432`; `store/fragindex.rs:374` | A batched index-status projection (identity-valid presence, outcome, summary) that never materialises the index and never turns corrupt rows into a ready badge; filesystem availability as a bounded cached observation with available/unavailable/unknown and a timestamp; the authoritative open stays at playback — §3.3.3 |
| C15 (Astra) | P2 | `emit_with_network` spawns a task per event, reads settings inside it (a consistent read on hiqlite), then does an individual insert on the serialised node-local writer; no bounded queue, and the event-derived counters live inside the raw-retention branch, so disabling retention disables the metrics | `telemetry.rs:336-360` | Bounded channel + supervised batching writer; cached effective settings; counters independent of raw retention; explicit coalesce/drop policy with room reserved for terminal outcomes and a shutdown drain; keep it node-local — §3.3.4 |
| C16 (Astra; owned elsewhere) | P1, already planned | Each rolling producer reserves the per-session ceiling + 64 MiB (2 GiB + 64 MiB at defaults) and `global_flow_bytes` charges `max(actual, reservation)` for live **and retired** presentations, so three reservations fill an 8 GiB cap regardless of bytes on disk; the refusal is a plain string where `session_start_error` expects the capacity classification | `transcode.rs:25890, 26001`; `hls.rs:3858` | Tracking item only: the seek-scratch RCA and implementation plan own this (both still untracked in the checkout at review time). Release future capacity only after all writers settle; keep retained objects and reader pins as separate obligations; classify the refusal; never recover space by deleting advertised segments (RFC 8216 §6.2.2) — §3.3.5 |


#### 3.3.1 C12 — bound and supervise scan probes

`scan/probe.rs:190-215` calls `Command::output().await` — no wall deadline,
no bounded pipe collection, no `kill_on_drop`. Tokio documents that a dropped
`Child` is not killed by default, so a timeout wrapped around `output()` is
not enough either. The decoder-fact probe next door is carefully supervised;
this one is not, and it is the one that runs over the whole library. Change:
route through the existing bounded primitive, minimal environment, kill and
reap on cancellation, admission held until cleanup finishes, typed failure,
scan continues. A blocked filesystem syscall needs bounded admission too;
cancelling its waiter does not cancel the OS call. Acceptance in the table.

#### 3.3.2 C13 — decoder-fact hashing and fallback provenance

Applies to speculative/offline production and immutable VOD preparation when
a probe identity is available; rolling start uses catalogue facts, so this is
not every play. What is confirmed is the mechanism: a warm hit still hashes
three executable-sized inputs under a process-wide gate, and the fallback
path does not distinguish "timed out" from "the source changed". Downstream
labels the fallback `CatalogRow`, so this alone does not prove wrong output
or a bypass of later fences. The earlier remediation ledger's A03 reserved
this optimisation for measurement; this pins the exact cost and
serialisation point. Acceptance: concurrent warm and cold starts with the
production static binary; p50/p95 gate wait and first-frame time; binary
replacement, same-size mutation, cancellation and source-change tests still
pass.

#### 3.3.3 C14 — detail reads and storage availability

Artwork and list batching already improved; this is the remaining detail
path. Acceptance: query instrumentation shows no bulk index payload for a
badge; detail stays responsive with an unavailable mount, a large audiobook
and several video identities; a stale availability observation never
authorises opening an invalid source.

#### 3.3.4 C15 — telemetry backpressure

Client ingest limits do not bound the internal lifecycle producers; under slow
storage, outstanding telemetry tasks grow and compete with fragment-index work
on the same serialised store. Source-confirmed risk, not a measured leak.
Acceptance: inject a slow/failed writer during playback; queue depth and
memory stay bounded, playback stays responsive, drops are counted;
retention, shutdown and network-prior opt-ins remain independent.

#### 3.3.5 C16 — scratch reservations stay with the existing repair

Disposition: already addressed by an approved repair plan
(`SEEK-SCRATCH-RESERVATIONS-RCA-AND-FIX.md`,
`SEEK-SCRATCH-RESERVATIONS-IMPLEMENTATION.md`, September 20 headers; not yet
on `main`). This is a tracking item, not new scope. The mechanism is
confirmed in the tree; the media1 incident it describes was not independently
reproduced here. Acceptance belongs to that effort: repeated seeks and
overlapping viewers under the unchanged 8 GiB cap, cancelled starts, open
segment readers, failed cleanup, true ENOSPC; a refused destination must
leave the incumbent playing; bytes and reservations counted separately.

### 3.4 Live TV and channels

| # | Sev | What | Evidence | Change |
|---|---|---|---|---|
| L1 | P1 | DVR rule expansion reads the guide through the 2 MiB *response* clipper — a 14-day horizon over 40–60 channels is 10–20 MB of JSON, so series rules only see the first day; and the whole guide is cloned + JSON-serialised twice per 15 s tick under the guide mutex | `dvr.rs:753-760, 848-857`; `guide.rs:222-279` | Give the scheduler an `Arc<LiveTvGuide>` view; apply the byte bound only in the HTTP handler; clip per channel by binary search; regression with a day-13 airing |
| L2 | P1 | Every live session does a leader-consistent read of the whole settings table once per second; a ~6 s raft hiccup ends the stream with text blaming the tuner | `live_tv.rs:5469-5475, 2740-2747`; `hiqlite.rs:3499-3509` | The read carries owner, generation, enabled state and drain admission — it is a fence, and a process-local `watch` fed only by this node's writes would miss changes made on another node. Share one bounded, validated observation across sessions on the node (one consistent read per second per node instead of per session), keep the replicated source of truth, define freshness, and bound the "read failed" grace explicitly rather than "continue" |
| L3 | P2 | Ingress builds a new `reqwest::Client` and runs a membership SQL query for **every** relayed playlist/segment (2/s per remote viewer) | `http/live_tv.rs:1093, 1121, 1356-1385`; `peer_transport.rs:50-58` | One `PeerTransport` in `AppState`; cache `owner_peer` per membership epoch |
| L4 | P2 | One tuner + one ffmpeg per *viewer*, default ceiling 2; the DVR fan-out primitive exists but viewers do not use it | `live_tv.rs:1253-1268, 267-269, 1402-1405` | `LiveTransport` keyed by `(device, channel)` fanning TS to N pumps; optionally share ffmpeg per plan |
| L5 | P2 | Library-channel guide/resolve clone the whole generation (≈720 KB) on every LRU hit, re-validate O(n) per call, up to 1,000× per guide page, under one process-wide `std::sync::Mutex` | `http/library_channels.rs:1299-1311, 941-963`; `library_channels.rs:993-1047` | Cache `Arc<Generation>`, validate once at insert, advance by ordinal |
| L6 | P2 | Every start spends a fixed 3 s collecting a prefix and ~0.5–2 s ffprobing it although the channel's format is cached for 20 min (docs: first segment 5.5–7 s) | `live_tv.rs:136-137, 5201-5296, 5632-5660` | 3 s is a ceiling (8 MiB or 3 s, whichever first), not a fixed wait, and the cached `source_format` lacks the codec/HDR/frame-rate facts the planner needs. Extend the cache to the full plan inputs with freshness and mux-change handling, then spawn on a warm cache; a later stderr demotion cannot undo an already-published wrong delivery |
| L7 | P2 | Bitrate/captions | §3.1 Q9 | — |
| L8 | P2 | `live_tv.rs` is nine modules in one 468 KB file; three parallel HLS playlist parsers in the tree | `transcode.rs:1107`, `renditiondir.rs:162`, `live_tv.rs:6487` | Mechanical split under `live_tv/`; one `hls_playlist` parser in `plurx-core` |
| L9 | P3 | A session whose scratch cleanup fails is never retired and counts toward the tombstone cap that refuses all starts; producer polls scratch every 250 ms with `read_dir` + per-file metadata; no `EXT-X-PROGRAM-DATE-TIME`; misleading "capability expired" after owner restart | `live_tv.rs:5071-5080, 6362-6477, 4784-4786` | Cleanup can fail because the child exit was not confirmed as well as because the directory would not delete — retire from *capacity* accounting while keeping a retry owner for reaping and scratch removal. One bounded blocking scan instead of per-file async stats, keeping the size/regular-file/deletion-lag checks (a playlist-mtime gate misses growing temp files). `+program_date_time` is a capability, not a stall fix. After an owner restart the lookup cannot distinguish restart from expiry — say "unavailable, press Watch" unless a persisted incarnation proves restart |
| L10 (Astra) | P2 | `pump_tuner_fanout` writes DVR sinks sequentially; each write may wait the full 30 s tuner timeout and the sink mutex is acquired outside that timeout, so one slow destination stalls the shared tuner reader and every sibling recording | `live_tv/dvr.rs:1914-1990` | Keep one tuner transport; give each sink an owned writer and a small bounded byte queue over shared immutable buffers; a destination that cannot keep up fails or rolls its own attempt; define overflow and cancellation before adding concurrency; keep generation fencing and distinct attempt files. Acceptance: two overlapping recordings, one injected slow writer — the healthy sink continues, the failing one records its gap, queues stay bounded, stop/failover waits for writers to settle |

The 1 s uniform-cadence fix and the persisted guide are correct — the two
documented Live TV incidents were fixed at the root, not the symptom.

### 3.5 Web client

| # | Sev | What | Evidence | Change |
|---|---|---|---|---|
| W1 | P1 | 2.6 MB uncompressed over 71 sequential HTTP/1.1 requests | §2.5 | gzip at startup; `<link rel=preload>` for the largest rows |
| W2 | P1 | `hls.min.js` is neither hashed nor `no-cache` (`max-age=604800`, no ETag) while app code subclasses its internals — a 7-day version-skew window on the one library whose private API is depended on | `web.rs:228-238`; `player.js:558-564` | Add it to `WEB_ASSETS` so it gets the hash + `immutable` |
| W3 | P1 | Every non-VOD seek reopens the server session — a ±10 s nudge on a transcode is a new ffmpeg; PLAYBACK.md calls this "open work" | `transport.js:733-773`; `decode-tiers.js:1122-1165`; `PLAYBACK.md:1573-1588` | Scope: rolling HLS and progressive remux only (direct play and immutable VOD already seek locally). Seek locally when the target, through `PLAYER.offset` and the current generation, lands in `v.buffered` or the *retained* published range — a published range is not a buffered one, and the holdback is a per-stream policy, not a fixed 1.5 s; put the decision in `playback-policy.js` and add both the local and the reopen-fallback case to `seek-control.test.js`. Android has the same gap |
| W4 | P2 | Worker off / no `recoverMediaError` | §3.1 Q10 | — |
| W5 | P2 | Seven sidecars (266 KB) are `no-cache` with no validator, re-downloaded in full per navigation; 196 KB of base64 fonts inside the render-blocking stylesheet (44 % of critical CSS) | `web.rs:241-328`; `app.css:2` | Hash them via the tag rewrite; serve `.woff2` as rows with `preload` |
| W6 | P2 | No CSP or security headers, 398 inline `on*=` handlers, bearer in `localStorage` and in every `<img src>` as `?token=` | `web.rs:197-199`; `core/api.js:70` | Now: `nosniff`, `Referrer-Policy`, `CSP: frame-ancestors 'none'; base-uri 'none'; object-src 'none'`. Then delegate handlers to one `data-action` listener and ship `script-src 'self'`. Escaping discipline (`esc()`) was checked and is consistent |
| W7 | P2 | No global error reporting; after the split a load-time throw is silent and nothing reaches `/client-log` | only `transport.js:527` handles `error`, on the `<video>` | `window.onerror`/`unhandledrejection` → `clientLog` with a cap; a boot sentinel banner |
| W8 | P2 | Structure: 62 scripts, one global scope, no types, only `node --check`; 166 of 427 commits were fixes, dominated by ownership/generation races on a shared untyped `PLAYER` bag; `play()` is 479 lines, `attachHls` 378 | `decode-tiers.js:606`; `player.js:499` | **Not** a bundler/ESM migration. `tsc --noEmit --allowJs --checkJs` in CI with a `jsconfig.json` listing the rows in served order (TypeScript models scripts as one global scope, which is the runtime truth) + ESLint `no-undef`, ratcheted from a baseline so existing diagnostics are not merge-blocking on day one; a few `// @ts-nocheck` and JSDoc edits will be needed, so "no source edits" was optimistic; the existing load-order and vm gates stay — a file list does not enforce runtime order. One JSDoc `@typedef Player`; then split `play()`/`attachHls()` along their own comment seams |
| W9 | P3 | Library grids repaint the whole page per 200-item batch, no poster size variants; progress heartbeat continues while paused; no MediaSession/media keys/TV back-key codes; poster hover ignores `prefers-reduced-motion` in one layout | `library-grids.js:24-159`; `decode-margin.js:517-521`; `stats.js:586-613`; `app.css:147` | Separate small fixes: append batches only where sort/filter ownership keeps order; `decoding="async"` and `?w=` variants; suppress the paused heartbeat only after tracing what else reads it (liveness?) and sending explicit pause/seek/end transitions; MediaSession with feature detection; TV back codes; a shared reduced-motion rule covering both `transform` and `transition` before deleting the two per-layout overrides |

### 3.6 Apple client

| # | Sev | What | Evidence | Change |
|---|---|---|---|---|
| A1 | P1 | No display criteria on tvOS | §2.8 | §2.8 |
| A2 | P1 | No audio-session handling | §2.10 | §2.10 |
| A3 | P2 | `PlayerController.swift` 9,548 lines, 250 methods, **nine** independent generation counters, 58 hand-written fence conjunctions, 39 unstructured `Task {}`, 32 fix commits (12 on one day) | `PlayerController.swift:1619+` | An immutable `Attempt` snapshot captured per continuation so the required checks are explicit — but the nine epochs have different invalidation scopes (a viewer action must not cancel an unrelated prepared replacement), so do not collapse them into one counter; compare the fields each continuation actually depends on. Then split by the seams the file already draws. Do after A1/A2 |
| A4 | P2 | Three independent AVPlayer stacks; the main one observes only `status` + end and never `FailedToPlayToEndTime`, `NewErrorLogEntry`, `timeControlStatus`; error classification reads `errorLog()?.events.last`, often a benign subtitle entry | `:6908-6977, 6990, 7258` | One `AVPlayerItemObserver` emitting a typed `AsyncStream<PlayerEvent>` for all three |
| A5 | P2 | `LibraryView` eagerly pages the whole library, re-sorts the merged array on the main actor after every page, filters three times per render — 6,000 titles = 30 main-thread sorts and 24k `localizedCaseInsensitiveContains` per keystroke | `AppModel.swift:611-644`; `LibraryView.swift:14-94` | Page on demand near the grid end; drop client re-sort; memoise with a 150 ms debounce |
| A6 | P2 | 25 / 50 / 100 ms `Task.sleep` poll loops on the main actor (~480 wakeups per quality change); a 2 s status poll for the life of every session including backgrounded audiobooks | `PlaybackControlSession.swift:568`; `PlayerController.swift:7990-7997, 6637-6713, 5011-5047` | Convert polls to KVO/continuations **keeping their deadlines** (the item-ready poll replaced a KVO-only wait that hung) and installing observation before reading state. The 2 s status poll feeds `observeDeliveryStarvation` and prepared-switch sampling, not just the panel — separate presentation telemetry from recovery evidence before backing anything off; do not key the cadence on panel visibility alone |
| A7 | P3 | Now Playing rewritten at 2 Hz (an XPC each); tvOS has no Now Playing / remote commands at all; no `AVRoutePickerView`; no scrub thumbnails; Swift 5 mode with no strict concurrency and one `@unchecked Sendable` (`Session`) with unguarded `origin`/`token` | `:6823, 8832-8847`; `project.yml:14-18`; `Session.swift:8-14` | Update on state change; enable remote commands on tvOS; `SWIFT_STRICT_CONCURRENCY: complete` as warnings, then errors |

### 3.7 Android client

| # | Sev | What | Evidence | Change |
|---|---|---|---|---|
| D1 | P1 | Buffer budget / no frame-rate matching | §2.9 | §2.9 |
| D2 | P1 phone / P2 TV | Backgrounding never pauses VOD (no ON_STOP on the player); no `MediaSessionService`/foreground service or wake mode for audio; `keepScreenOn` unconditional | `PlayerScreen.kt:1089-1097, 1314`; `Controller.kt:2640-2647`; manifest | ON_STOP + not PiP → pause video (owner-initiated, preserving an explicit user pause across the transition); audio-only → `MediaSessionService` + notification + `WAKE_MODE_NETWORK` + the foreground-service permission, as one contract; keep `keepScreenOn` for video (a wake lock does not keep the display on) but drop it while paused |
| D3 | P2 | Live TV treats `ERROR_CODE_BEHIND_LIVE_WINDOW` as fatal; AudioTrack failures (5001/5002/5004) never enter the compatibility ladder although the route-aware probe exists for exactly that; node failover fires on 401/404/410 because the "filtered at the call site" comment describes code that does not exist | `LiveTvPlayer.kt:481-488`; `PlaybackPolicy.kt:77, 101-120`; `Controller.kt:704-707, 2280-2331` | `seekToDefaultPosition(); prepare()` once per attach for real live streams, after checking the session/window is still valid; classify 5001/5002/5004 explicitly — some are route/device state, not media incompatibility — capture sink diagnostics, re-snapshot caps and spend the existing compatibility budget rather than transcoding forever for a disconnected output; gate failover on the `InvalidResponseCodeException.responseCode` |
| D4 | P2 | Three of four `ExoPlayer.Builder` sites skip audio focus, becoming-noisy, tunneling and decoder fallback | `LiveTvPlayer.kt:164-167`, `LibraryChannelPlayer.kt:65-68`, `OfflineDownloads.kt:391-395` | One `PlurxPlayerBuilder(context, live)` |
| D5 | P2 | "Open in…" hands the account bearer to arbitrary apps via `?token=`; bearer in plaintext DataStore included in cloud backup | `DetailScreen.kt:486-491`; `Session.kt:118-123`; `SettingsStore.kt:14,39,142`; manifest `allowBackup="true"` | Server-minted short-lived file grant; exclude `datastore/` from backup (one line) |
| D6 | P2 | One OkHttp dispatcher (5/host) shared by Coil and Retrofit — `/decision` queues behind 24 posters; whole-library download with per-page main-thread re-sort; `Controller.kt` (4,293 lines) never instantiated by any test; sideload path builds `assembleDebug` (no R8, `run-as` exposes the token, no Baseline Profile) | `Net.kt:23-38`; `AppViewModel.kt:674-681`; `Makefile:1713-1751` | Four independent tasks: a separate dispatcher for Coil (trace the contention first; the 5/host limit is for async calls only, and image concurrency above the server's eight-permit gate just moves the queue); sort off-main and preserve merged order across libraries (pages are clamped to 200 server-side); a `PlayerPort` boundary with focused controller tests; release signing + Baseline Profile with the deployed variant measured |
| D7 | P3 | `Application.onCreate` does two `runBlocking(IO)` reads for offline books TV never uses; 2 s session-status poll for the life of every session regardless of panel | `OfflineDownloads.kt:113-141`; `Controller.kt:2225-2256` | Move the offline-media recovery off the main thread while keeping process-wide readiness for pending transfers (it is offline media, not only books, and TV is not proven to never use it); the status poll is capability-scoped, not account-authenticated — back it off only with a replacement detection bound after auditing its other readers |

---

### 3.8 Native adaptive quality — the browser has a policy, the native clients have none (Astra; P2, L)

The browser has an enabled-by-setting, evidence-aware adaptive controller
(`player/stall-diagnosis.js:369`) over a pure rung policy
(`playback-policy.js:299`). No Swift or Kotlin source consumes
`playback_auto_abr` or runs an equivalent throughput/runway loop. The legacy
stall-ticket plumbing for requesting a lower rung is dormant: Apple's
`stallReopenIntent(wedge:)` (`PlayerController.swift:5683`) has no call site,
and Android's reopen payload leaves `previousSessionId`/`reopenReason` at
their `null` defaults (`SubtitlePolicy.kt:363-389`) — confirmed. Android's
`onStall` (`Controller.kt:1837`) correctly refuses to treat every stationary
presentation as bandwidth evidence; keep that. Change: share policy fixtures
and evidence semantics across platforms over each player's real
measurements; distinguish constrained delivery, producer capacity, decoder
failure, deliberate hold and denied authority; use the prepared handoff for
healthy changes and bounded recovery when the incumbent is already stalled;
hysteresis, a switch budget, safe upward recovery; honour Original, manual
quality, paused state and HDR fidelity. A single-rendition manifest does not
become a multi-rung service because AVPlayer or Media3 supports ABR.
Acceptance: the same shaped-network trace on browser, Apple TV/iPhone and
Android TV/phone, comparing stalled seconds, switches, first-frame gaps,
quality regained and unexpected SDR transitions. Not enabled on a pure policy
test alone.

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

Of 861 `#[cfg(test)]` attributes in `src/`, 180 introduce a `mod tests`; the
other 681 are a mix of test-only imports, helpers, types **and** fields and
branches inside shipping structs — `transcode.rs:3290-3293`
`wait_before_await_pause: Mutex<Option<Arc<Barrier>>>`, three pause fields on
the attempt supervisor, and their siblings in `playback_control.rs`. The
first draft called all 681 "seams in shipping structs"; that needs a
syntax-aware census before it is a number. The real ones matter: struct
layouts, lock acquisitions and `.await` cancellation points differ between
test and release where they sit, so those supervisor orderings are proven for
a test-shaped state machine. The repo already has the right precedent
(`scratch-fault-injection` feature, `PLURX_CLUSTER_ACTIVATION_FAILPOINT`): an
injected `Hooks` trait or `fail`-crate failpoints, no-op in production.
Classify the seams first, migrate the lifecycle ones, keep the deterministic
race tests they enable. (M–L)

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
the "fallback" sentence deleted) — the two are not exclusive, and a rename is
optional. Either way, remove the unconditional `s3` feature edge in the
fork's `Cargo.toml:190` (`default-features = false` alone does not disable an
explicitly requested feature; cryptr's defaults are already empty), make it
`s3 = ["backup", "cryptr/s3"]`, delete `vendor/s3-simple` once no optional
combination needs it, and ban the *duplicate* `aws-lc-sys` versions in
`deny.toml` rather than the crate outright — the TLS provider choice (ring vs
aws-lc) is a separate decision with its own feature matrix.
Same shape for semantic search: candle + gemm ×7 + rayon + Oniguruma C build
compiled into every `cargo check` for a 423-line optional feature
(`plurxd/Cargo.toml:26-30`) — `tokenizers` → `fancy-regex` backend (benchmark
tokenizer equivalence first) and a `semantic-search` feature, or a spawned
`plurx-embed` process per the isolation design. If the feature is off in the
fast lane and on in Docker, a required compile check must still cover the
shipped feature set, or that is one more configuration that reaches
deployment uncompiled.

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
`<path>::<name>`" field that is bound to the merged tree and checked once at
merge (define that binding before deleting the receipts, not after);
corrective = `fix(`/`perf(` prefix only, accepting that mislabelled subjects
escape; prune text-contract tests one at a time, each with a demonstrated
stronger runtime replacement (keep the security-posture ones, and keep
`test_docs_index` and `test_status_pr_claims` — a link checker enforces
neither inventory nor status truth); move status prose out of STATUS.md
(2,581 lines) into per-effort docs with a one-line index row. The numbers
above are under a broad "corrective" rule; the assessor's `^fix(` rule gives
1,037 ledger-touching commits and 134 fix commits on operations tests — same
conclusion. Production counters (§5.3) supplement regression tests; they do
not replace them. (M)

### 4.5 Release versioning stopped (rollback artefacts exist)

Last semantic tag v0.3.0 (2026-08-31); `Cargo.toml` still 0.3.0;
`[Unreleased]` is 340 lines for ~3,100 commits; the fleet runs
`v0.3.0-700-g…`. The first draft said there was no rollback artefact — wrong:
OPERATIONS.md §308 documents immutable `sha-<12hex>` image tags with the ten
newest retained, so a deploy can be rolled back to a named image (schema
compatibility permitting, which is a separate limit). What is missing is the
*release* layer: semantic tags, CHANGELOG rollover, and a mapping from what a
node runs to what the CHANGELOG says — the fragment-index outage, the
HDR-x264 regression and four schema hotfixes all fall inside one unreleased
span. Mobile counters were bumped 92 times because `mobile_versions.py`
enforces it; nothing equivalent exists for the server. Options for §7: a
`v0.3.x` tag per fleet deploy would also fire the full tag CI you disabled
elsewhere, so either accept that cost, or tag weekly from a green scheduled
run and keep the `sha-` receipts as the deploy identity. (S)

### 4.6 Ops hardening that is missing

`deploy/plurxd.service` has `NoNewPrivileges`, `ProtectSystem=strict`,
`ProtectHome` and nothing else: no `LimitNOFILE` (systemd soft default 1024
and tokio does not raise it — each session holds pipes, the source fd, up to
seven sidecars, segments and sockets; the hiqlite patch log already records an
fd-exhaustion incident), no `OOMScoreAdjust` (the OOM killer may pick the
daemon over a wedged x265 child), no `TasksMax`/`MemoryHigh`/`PrivateTmp`;
Compose has no `ulimits`/`pids_limit`. Children run at the daemon's priority
(`grep setpriority|ionice|oom_score_adj` in non-test src → none). Fix, each
validated against the fleet's actual inherited limits and observed fd demand
first: `LimitNOFILE=65536`, `OOMScoreAdjust=-500` (inherited by children, so
the child `pre_exec` must set `oom_score_adj=+500` back — and `pre_exec` must
stay async-signal-safe: no allocation, no logging), `TasksMax=4096`,
`PrivateTmp` (check existing scratch paths), `ProtectKernelTunables
RestrictSUIDSGID LockPersonality`; child `setpriority(10)` and `ioprio BE/7`
checked against realtime delivery; raise soft NOFILE at startup. Also: CI unit
tests run against ffmpeg 6 while the shipped image asserts jellyfin-ffmpeg 8
capabilities (`ci.yml:267`; the action's own comment: "CI never caught it
because CI has never run ffmpeg 8"); the `Dockerfile` base `rust:1-bookworm`
floats — the compiler itself is pinned via `rust-toolchain.toml` and rustup,
so this is system-library drift, not compiler drift; release profile is
`lto="thin"` and `strip="symbols"` with no debug info, so native stacks are
unsymbolicated (panic *locations* still print). Corrected profile proposal,
each line a measured trade-off, not a free win:

```toml
[profile.release]
lto = "fat"                 # measure build time and binary size first
codegen-units = 1           # same
debug = "line-tables-only"  # symbolicated stacks
strip = "none"              # "debuginfo" would remove the line tables above; ship split debug files if size matters
overflow-checks = true      # changes runtime behaviour: test before enabling
```

`panic=unwind` itself is correct for the isolation design, keep it. (S each)

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
marks 44 docs "open" whose own `**Status:**` header says merged or complete
(verify each before changing it — a merged PR does not close its effort). The
first draft's "51 unindexed files" was wrong: they sit in folders the index
test exempts by policy (`apple-builds/`, `archive/retro-*`, `evidence/`). One
PR: rewrite §2.1/§3/§6/§8/§9 with named constants and file links (as
`transcode/mod.rs:68` already demands: "Nothing may hardcode the number"),
record the DVR reversal against the existing LIVE-TV-DVR-IMPLEMENTATION and
LIVE-TV-DVR-AND-REMINDERS-OPTIONS documents rather than as an undecided
question, and extend `validation/doc_versions.py` to check the constants the
doc quotes. (S)

---

### 4.8 Two policy tests are red on `main`, and one of them is a stale call count (Astra; reproduced)

`node tests/playback/web-policy.test.js` prints 137 PASS lines and fails at
`web-policy.test.js:6007`, which expects two literal
`behindLiveWindow.attached()` calls in Android; there is now one, inside
`attachRecipe` (`Controller.kt:373`), which the prepared commit
(`Controller.kt:3565`) calls. The source-count assertion is stale; it does
**not** show that Android lost its recovery-budget reset. Reproduced here.
`node tests/playback/web-control.test.js` also fails here under Node
22.22.2 at `:3164` (`true !== false`); Astra's Mac run on Node 26.8.1
passed, so this one is ordering/timing under that runtime — investigate
before calling the suite green, and do not "fix" it by widening a timeout.
Both are the same lesson as §2.2: nothing runs these on merge. Change:
replace the call count with a behaviour test (spend a recovery, attach a
prepared successor, exactly one new recovery allowed); keep inventory tests
for discovery and ownership but state their limits — they establish neither
frame continuity, interlace handling, receiver output nor cancellation
cleanup.

### 4.9 Make resource and presentation ownership reviewable (Astra)

The target is unchanged — one deployable server — with smaller interfaces
between owners that already exist:

```text
 catalog + source facts + device/route capabilities
                       |
                       v
              resolved media contract
     video / audio / subtitles / transport / identity
                       |
                       v
        admission ---- producer attempt owner
                           |
                           v
                 published-object owner
                 visibility / grace / pins
                           |
                           v
                 client attachment owner
                prepared -> visible -> retired

 telemetry <- bounded observations from each owner
 durable state <- authoritative mutations, not per-frame events
```

A producer stopping, a playlist retiring, a reader closing and a player
showing its first frame are four different events; treating one as proof of
another is the pattern behind §2.6, C16, L9 and several of the month's
lifecycle fixes. This is the frame for the §4.1 decomposition: extract along
these ownership boundaries, preserve the current fences and receipts, and do
not introduce a second controller. For performance decisions record p50/p95/
p99 first-frame time, seek-to-moving-picture, stalled seconds per hour,
failed starts per attempt, replacement failure rate, admission wait,
actual vs reserved scratch and bytes per watched minute, segmented by
client/transport/codec/grade/hardware with bounded labels and no content
identifiers. The telemetry exists; this turns it into comparable release
evidence.

## 5. Sequencing — what I would do, in order

Revision 2 splits the items the assessment showed were really two or three
changes with different tests, and moves anything whose remedy is now a design
question out of the week list. Nothing below depends on a withdrawn remedy.

### 5.1 This week (all S, all verified; ~14 small PRs, one change each)

1. `output_job_owned` pipes both streams; portable child test covering
   stdout, stderr, non-zero exit and cancellation (§2.1).
2. **Your call first (§7.1)**, then either `make unit` + workspace clippy in
   the fast Rust gate, or a scheduled `ci.yml` on `main` with heavy lanes
   excluded and schedule events cancellable, or both (§2.2).
3. `ReaderStream::with_capacity(256 KiB)` at the four body sites, with a
   throughput/memory measurement in the PR body (§2.4 part 1). The pump-ack
   batching is a separate later PR (§2.4 part 2).
4. Listener timer + header-read timeout + per-group `TimeoutLayer` (§2.5 PR 1).
5. gzip `WEB_ASSETS` with proper negotiation and `Vary`; `hls.min.js` into
   `WEB_ASSETS`; sidecars hashed or validated (§2.5 PR 2, W2, W5).
6. Security headers: `nosniff`, `Referrer-Policy`, `frame-ancestors 'none'`
   on the shell, checked against the reader/offline embeds (W6-now).
7. Font attestation: move the `stat` work off the runtime (§2.7 part 1). The
   frozen-font-environment design is §5.2.
8. Encoded-VOD hold: SIGSTOP + low-water mark + stopped→release on
   `Admissions::live_is_waiting`, with the late-arrival case in `prodsched` /
   `prodexec` tests first (§2.6).
9. Watched-outbox: local hint + keep the atomic claim, skip when unconfigured
   with bounded refresh, idle backoff (S3); TMDB/AniList deadlines incl. body
   and retry budget (C5).
10. tvOS: `preferredDisplayCriteria` from the asset via the active window
    (§2.8); Apple audio-session interruptions and route changes (§2.10).
11. Android: exclude `datastore/` from **both** backup rule files (D5 part 2);
    gate node failover on the HTTP status (D3 part 3).
12. Web: `enableWorker` on after exercising the fallback in the vendored
    build (Q10 part 1); global error/unhandled-rejection reporter with
    redaction and a cap (W7).
13. Live TV: full-guide view for the DVR scheduler with a day-13 test (L1);
    one `PeerTransport` in `AppState` (L3 part 1).
14. ARCHITECTURE.md rewrite with the DVR reversal recorded (§4.7).
15. **(Astra)** Bounded ordinary scan probes through the existing primitive
    (C12); replace the stale `web-policy.test.js:6007` call-count assertion
    with the behaviour test and diagnose `web-control.test.js:3164` under
    Node 22 (§4.8). C16 stays with the seek-scratch effort.

Deferred from the first draft's week list: `bounded_replica_reads=true` by
default (consistency-policy change with per-route coverage → §5.2); Android
heap floor (needs the device memory measurements in §8 first); systemd
limits (needs the fleet's inherited limits observed first; the unit edit
itself is trivial once they are); `v0.3.1` tag (your call on tag-triggered
CI, §7).

### 5.2 This month (M) — each its own PR with its own oracle

Encoder work **by invariant, not as one bundle**: CRF/QVBR vs ABR per encoder
family with a VMAF/bitrate comparison on representative content (Q1);
tone-map peak/primaries/dither after the hardware-download metadata test
(Q3); deinterlace with field-order capture and a chosen frame/field policy
(Q4); 5.1/EAC3 negotiation carried through recipe identity and manifests (Q5);
honest master BANDWIDTH/CODECS/RESOLUTION with the SDR-CODECS ruling
re-qualified (Q7). B-frames are **not** in this list — they are a timeline
design item (Q2). Cluster backup/restore as a procedure with a drill (§2.3).
Snapshot cadence from measured build time; off-writer snapshot only with the
single-cut design written down (S2). `bounded_replica_reads` per route (S1).
SQLite: `with_read` migration, `user_for_token` split, index pass from
`EXPLAIN QUERY PLAN`, `ANALYZE` (S6, S7), search predicate via current FTS
membership (F-sc-8, corrected). Frozen font environment per recipe (§2.7
part 2). Android `Display.Mode` matching, background/foreground-service
model, shared player builder (§2.9, D2, D4). Web local seek within the
retained published range for rolling HLS (W3) and `tsc --checkJs` ratcheted
from a baseline (W8). Apple `Attempt` snapshot with per-scope comparison and
one item observer (A3, A4). **(Astra)** Detail-path index summary and bounded
availability (C14); telemetry backpressure and cached settings (C15); DVR
sink isolation (L10); measure C13's gate wait and fallback provenance before
touching parser identity; audio resolved independently of video (Q5,
§3.1.2) and field order in the media contract (Q4, §3.1.1) are part of the
encoder-by-invariant series. Live TV shared transport per channel with
slow-consumer eviction (L4); warm-start cache extended to full plan inputs
(L6). Process: PR-level regression field with tree binding designed before
receipts are retired; text-contract pruning one test at a time (§4.4).
hiqlite `s3` feature edge and duplicate-crate cleanup (§4.3).

### 5.3 This quarter (L)

The `transcode.rs` / `hls.rs` / `vodserve.rs` extraction as behaviour-
preserving moves first, registry unification evaluated separately afterwards
(§4.1); `playback_control.rs` and `membership.rs` as explicit transition
functions with their contractual orderings preserved; the classified
test-seam migration (§4.2); the hiqlite fork decision (§4.3); the
"Replicated-ephemeral" tier corrected in the architecture now and designed,
if at all, later (S8); a clock-skew guard built on a real measurement (S9);
B-frames as a VOD timeline change (Q2); native adaptive quality (§3.8) and
the next codec/GPU qualification (Q12) as measured, fleet-driven work. And
one rule for every future effort,
which would have shortened five of this month's six fleet incidents: **the
exit criterion adds a counter read off `/metrics` on the fleet** — with its
event, bounded labels, expected-demand denominator, observation interval and
alert owner defined — alongside, not instead of, the pre-merge tests. Existing
series to build on: `plurx_live_tv_starts_total{outcome}`, the store and
snapshot histograms; the others named in the first draft were illustrative.

---

## 6. Already good — do not "fix" these

These are preservation constraints on the changes above, established by
reading the paths named; "no X anywhere" claims below are scoped
observations from spot checks and grep, not proofs, and the review itself
found exceptions to two of them (the unpiped helper; the VOD spawn outside
the Windows job helper).

- Zero `unwrap()` in ~315k first-party production lines (`unwrap_used` + `-D warnings`);
  exact toolchain and SHA-pinned actions; rustls-only, no openssl.
- Store discipline: rusqlite strictly on `spawn_blocking`, WAL +
  `synchronous=NORMAL` + `busy_timeout` + FK on, STRICT tables, append-only
  migrations with per-migration FK check and newer-schema refusal; the
  progress coalescer and its physical-commit budget test; lease renewals
  batched into one raft txn per node per tick; `json_each(?)` for ID lists;
  `FILE_COLS` excluding `probe_json`; `TimedClient` as the sole hiqlite path
  with the bypass-forbidding test.
- No `std::sync` guard held across `.await` found in the server-core files
  read; every per-session/per-user map checked is TTL-pruned or capped; auth
  extractors applied uniformly across the 205 `/api/v1` route registrations
  read from the router (nested handlers and capability paths were not
  exhaustively audited); timing-uniform login with a real dummy hash; `openat` +
  `O_NOFOLLOW` file serving; overflow-safe range parsing; browse endpoints
  paginated and batched.
- Process supervision on the rolling and live paths: `kill_on_drop`, owned
  stderr readers with a bounded ring, biased reap-before-signal select,
  Windows job objects, SIGSTOP pacing through the supervisor, `statvfs`
  emergency margin, working-set hysteresis. (The VOD spawn and the
  `output_job_owned` helper are the two paths outside it — §4.1, §2.1.)
- Copy/remux path: `hvc1`/`dvh1` tagging, `dovi_rpu=strip`, `filter_units`,
  `-map_chapters -1`, `-noaccurate_seek` + `avoid_negative_ts make_zero`;
  `OutputGrade` typing that makes PQ-at-8-bit unspellable; forced-IDR per
  family; `-maxrate 1.5×/-bufsize 2×` with measured rationale; 6-ch AAC
  forced to `channel_layout 5.1`; the VOD frame-grid design and
  `establish_or_verify`; fragment index keyed by the executable closure
  digest.
- HLS authoring for AVFoundation: VIDEO-RANGE/SUPPLEMENTAL-CODECS and version
  10 only when needed, HDR10 SEI promoted into `hvcC`/`mdcv`/`clli`, forced
  renditions `DEFAULT=NO`, no INDEPENDENT-SEGMENTS on open-GOP copies. (The
  SDR-variant CODECS omission is Q7, not a positive.)
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
  MediaCapabilities HEVC tiering with PQ, `esc()` discipline (a convention, not
  an injection proof), generation-guarded rendering. (Android's good player
  defaults are on the main builder only — D4; the hashed-asset guarantee is a
  cache-busting URL, not a server promise about old hashes.)
- Process: RCA-grade commit bodies (`120a3d29`, `cfba5ff6`, `6febf93e`,
  `57e03a6b`, `93beae93`, `e314a5bd`); every `#[ignore]` read carries a reason;
  tests were added far more than deleted in the window; the byte-identical
  web-shell split with its gate; `PLURX-PATCH.md` for every vendored patch;
  mobile version enforcement.

---

## 7. Decisions that are yours

1. **Fast lane scope.** Your 09-10 ruling made the PR lane compile-only and
   disabled runtime schedules; the consequence is that no Rust test runs on
   any automatic trigger and the "batch fixer" has no input. Options: `make
   unit` (+ workspace clippy) in the merge gate at ~2–4 min warm; a scheduled
   `ci.yml` on `main` with heavy lanes excluded; or both. I recommend both —
   a schedule alone leaves a window where a red merge deploys. This also
   conflicts with your 09-17 instruction that the fast lane is the one place
   tests run, so it needs reconciling either way.
2. **hiqlite**: own the tested patch stack and upstream the generic fixes —
   the options are not exclusive; a rename is optional. (§4.3)
3. **Encoder defaults**: not one bundle and not one flag at a time behind a
   toggle — one PR per invariant (rate control, tone-map, deinterlace, audio,
   manifest), each with its own measurement, feature-gated only where
   operational rollback warrants it. VMAF covers picture, not audio or
   timing. B-frames are a separate design item.
4. **The ledger**: retire per-commit receipts for a PR-level field bound to
   the merged tree — designing that binding first? (§4.4)
5. **Semantic search and Windows**: semantic search has a clean compile
   feature boundary and should get one (with the shipped set still compiled
   by a required check). Windows is a platform target, not an optional
   dependency — keeping or dropping it is a product decision; its CI cost is
   evidence for that decision, not a reason by itself.
6. **DVR vs §8**: the architecture's non-goal was reversed on 2026-09-13; the
   retention/conflict/ownership decisions already exist in
   LIVE-TV-DVR-IMPLEMENTATION and LIVE-TV-DVR-AND-REMINDERS-OPTIONS —
   reconcile §8 to them.
7. **Release tags vs tag-triggered CI**: a semantic tag per fleet deploy
   fires the full `ci.yml`; weekly tags from a green scheduled run, or per
   deploy and accept the cost? (§4.5)

## 8. Open questions only the fleet can answer (prompts for GPT)

None of these was run for this review; each names the observation that
would settle it, not just the question.

- Apple TV in "4K SDR + Match Dynamic Range + Match Frame Rate": play a 24p
  HDR10 title and record the TV/receiver's actual HDMI mode before, during
  and after playback and across a quality change. The player badge is not
  evidence. (Decides §2.8's user impact.)
- iPhone: take a phone call mid-film; unplug headphones mid-film; also user
  pause during an interruption and a background/PiP transition. Record play
  intent, AVPlayer state, and whether a second server session appears in
  Activity. (§2.10.)
- `journalctl -u plurxd | grep "spawned a producer generation"` for one
  named encoded session over a two-hour film, counted per rendition with
  timestamps, plus the ahead target and whether any other session competed.
  A global count includes unrelated starts. (§2.6's rate.)
- `plurx_raft_snapshot_seconds{operation="build"}` bucket rates per node
  over a day, correlated with apply latency, lease renewals and DB/WAL size
  on disk. (S2's severity.)
- `plurx_store_operation_seconds{class}` p50/p99 by backend and role on the
  three-node lab vs single node, with call counts per route — store-call
  duration is not page latency. (S1.)
- Size of `guide.json` on media1 **and** a diff between the scheduler's
  filtered window and the full source for a known airing 10+ days out; a big
  file alone does not prove the scheduler's window is clipped. (L1.)
- `ffmpeg -hwaccel qsv … -vf hwdownload,format=p010,showinfo` on an HDR10
  fixture on media1: are MDCV/CLL, `color_trc`, primaries and matrix still
  present after download? Repeat with a file that has no MaxCLL. (Q3.)
- `memoryClass` / `largeMemoryClass` **and** real process memory, decoder
  allocations and image-cache size during playback on the Lenovo, Google TV
  and Shield; with and without a primed successor. (§2.9.)
- Inventory configured vs actually-selected encoders per node from
  `/metrics`; does any production session use NVENC or VideoToolbox? (Q6.)
- A known DV Profile 5 title played through a transcode path since 09-14:
  client logs should show `unsupported_build_error` with the "did not prove
  … changes pixels" text. Existence of P5 titles alone is not impact. (§2.1.)

## 9. Verification record for the revision-3 additions

What was run, and what was not. No Rust, Swift or Kotlin was changed; no
compiler, broad runtime suite, device playback, load test, VMAF run or
restore drill was performed for this document.

| Check | Result |
|---|---|
| Tree | `main` @ `a1414368` for every code citation; documents landed at `0afefd92` (rev 1), `ef37b7cc` (rev 2), and this revision — no runtime change in between |
| Astra's C12 | `scan/probe.rs:200-213` `.output()` with no `kill_on_drop`/timeout on that call (the supervised one at `:129-144` is the reporter probe) — confirmed |
| Astra's C13 | `decode_facts.rs:26` 512 MiB image bound, `:464 validate_current`, `:2887-2931` `probe_gate` of 1 permit acquired before cache — confirmed |
| Astra's C14 | `browse.rs:394` sequential `tokio::fs::metadata`, `:432` `fragment_index(...).is_some()` per identity — confirmed |
| Astra's C15 | `telemetry.rs:336-352` `tokio::spawn` per event with `get_setting` inside — confirmed |
| Astra's L10 | `dvr.rs:1914-1990` sequential `for sink in sinks`, `sink.file.lock().await` outside the `timeout(TUNER_READ_TIMEOUT, write_all)` — confirmed |
| Astra's Q12 | `encoder.rs:239-252` `video_codec_for`: HDR10 → `libx265`/`hevc_qsv`, `None` for NVENC/VAAPI/VideoToolbox — confirmed |
| Astra's §2.8 note | `LibraryChannels.swift:855, 901` `VideoPlayer(player:)` — confirmed |
| Astra's §3.8 | `stallReopenIntent(wedge:)` defined at `PlayerController.swift:5683`, no call site; Android `previousSessionId`/`reopenReason` default `null` in `SubtitlePolicy.kt:363-389` — confirmed / LIKELY (callers not exhaustively read) |
| `node tests/playback/web-policy.test.js` | 137 PASS then `strictEqual` expected 2 at `:6007` — **red on `main`**, reproduced |
| `node tests/playback/web-control.test.js` | fails at `:3164` (`true !== false`) under Node 22.22.2 here; Astra: passes on Mac Node 26.8.1 — runtime-dependent, red here |
| Interlace fixture (§3.1.1) | Re-run on ffmpeg 6.1.1 (Astra: 9.0.1): current chain → `field_order=progressive`, 30000/1001, idet 90 TFF / 0 progressive; with `bwdif=send_field` → 60000/1001, idet 4 TFF / 176 progressive — identical counts |
| Seek-scratch documents (C16) | `docs/streaming/SEEK-SCRATCH-RESERVATIONS-*.md` present only as untracked files in the checkout; not on `main` |

**Interlace reproduction** (generated media only; the filter chain, not the
server):

```bash
mkdir -p /tmp/plurx-audit-media
ffmpeg -hide_banner -nostdin -y \
  -f lavfi -i 'testsrc2=size=640x480:rate=60000/1001:duration=3' \
  -vf 'tinterlace=mode=interleave_top,setfield=tff' \
  -c:v mpeg2video -flags +ilme+ildct -q:v 2 -an \
  /tmp/plurx-audit-media/interlaced.ts

ffmpeg -hide_banner -nostdin -y -i /tmp/plurx-audit-media/interlaced.ts \
  -vf "scale=-2:'min(720,ih)',format=yuv420p" \
  -c:v libx264 -preset veryfast -profile:v high \
  -b:v 4000k -maxrate 6000k -bufsize 8000k -an \
  /tmp/plurx-audit-media/current.mp4

ffmpeg -hide_banner -nostdin -y -i /tmp/plurx-audit-media/interlaced.ts \
  -vf "bwdif=mode=send_field:parity=auto:deint=interlaced,scale=-2:'min(720,ih)',format=yuv420p" \
  -c:v libx264 -preset veryfast -profile:v high \
  -b:v 4000k -maxrate 6000k -bufsize 8000k -an \
  /tmp/plurx-audit-media/deinterlaced.mp4

for f in current deinterlaced; do
  ffprobe -v error -select_streams v:0 \
    -show_entries stream=field_order,avg_frame_rate -of csv=p=0 \
    /tmp/plurx-audit-media/$f.mp4
  ffmpeg -hide_banner -nostdin -i /tmp/plurx-audit-media/$f.mp4 \
    -vf idet -an -f null - 2>&1 | grep 'Multi frame' | tail -1   # read the FINAL summary
done
```

The counts establish a regression fixture; they do not establish subjective
quality, a performance budget, or the right policy for mis-flagged or
telecined sources.

---

*Companions: `ARCHITECTURE-REVIEW-2026-09-20-APPENDIX.md` — the nine full
area reports, unrevised, with every finding, quoted evidence, per-area
"already good" lists, fix-density tables and open questions; the adversarial
assessment (`ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md`) whose
dispositions revision 2 applies; and Astra's independent review, whose
additions revision 3 merges in full — every finding, subsection, acceptance
criterion and experiment it added appears above under its id, so it is not
reproduced as a separate file; its own copies of the first-draft remedies are
superseded by this document. Where the appendix and
this document disagree, this document wins.*
