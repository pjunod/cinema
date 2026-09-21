# Architecture document reconciliation — make §1–§9 describe the tree it ships with

**Status:** ready for review · **Executes:** §4.7 / F-hist-10 / F-ltv-10 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `0f02b7ea`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Companion to [ARCHITECTURE.md](../ARCHITECTURE.md) (the document being
repaired), [VALIDATION.md](../VALIDATION.md) (how a check becomes a gate) and
[LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md](LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md)
(the same lane, the same preflight job). Read review §4.7 and §7.6, then
assessment rows `4.7`, `F-hist-10` and `F-ltv-10`, then §2 of this document —
which is the verification, claim by claim, with the constant and its line.
Board id **P-04**.

Work it in order: **M1 lands the guard before M2 rewrites the prose**, so the
rewrite is the first thing the new check passes rather than the last thing it
is written around. M3 and M4 are independent.

The standing instruction: **if a step seems to require changing a constant in
`crates/` so the document can keep its sentence, stop and flag it.** The code
is the truth here; this plan moves the document, never the tree. The one
permitted code edit is prose: three source comments that say "sixty-two" where
the array has sixty-four rows (§3.4).

---

**Correction to the review:** four.

1. **The "unreplicated SQLite mode" is not a supported deployment mode.**
   §4.7 lists `"1-voter raft single node" vs the unreplicated SQLite mode` as
   one of the wrong numbers. `crates/plurxd/Cargo.toml:34` depends on
   `plurx-core` with `features = ["hiqlite-store"]` unconditionally, and
   `select_daemon_store` (`crates/plurx-core/src/cluster/migration.rs:319-416`)
   activates a replicated store on a fresh data directory. The only
   unreplicated-SQLite boot is the **one recovery boot after an interrupted
   activation** (`:386-399`, `SelectedBackend::SqliteRecovery` at `:214-216`),
   which `crates/plurxd/src/main.rs:1342-1349` logs as a warning and which
   [OPERATIONS.md](../OPERATIONS.md):556 documents as a surface to restart out
   of. So §2.1's "single-node mode is the same code path with a 1-voter raft"
   (`ARCHITECTURE.md:88`) is **correct**; what is missing is the recovery boot
   and its one-way activation. §3.1 adds it rather than replacing the
   sentence.
2. **The web app is sixty-four files, not sixty-two.** `WEB_ASSETS`
   (`crates/plurxd/src/http/web.rs:57`) has 64 rows;
   [WEB-SHELL-LAYOUT.md](../clients/WEB-SHELL-LAYOUT.md):1 already says
   sixty-four. Three source comments still say sixty-two — `web.rs:8`,
   `tests/web/asset-graph.js:5`, `tests/web/shell-source.js:7` — which is the
   same prose drift as ARCHITECTURE's, one layer down. M4 fixes them.
3. **The watchdog claim is wrong twice, not once.** §4.7 gives
   `ACTOR_HARDWARE_STARTUP_BUDGET = 12 s` against the document's 8 s. The
   ownership changed too: the two budgets in `crates/plurxd/src/transcode.rs`
   are labelled "compatibility mirror … it sizes only the HTTP playlist wait;
   it is not a timer or recovery owner" (`:289-295`), and "the prepublication
   actor exclusively commits the 12-second verdict". §3.1 rewrites the
   mechanism, not just the number.
4. **The DVR options document's own header is stale.**
   `features/LIVE-TV-DVR-AND-REMINDERS-OPTIONS.md` still reads
   `**Status:** decision needed` although its recommendations were accepted
   2026-09-13 — the sibling implementation document says so in its own status
   line. §3.2 records the acceptance in that header as part of the same PR
   that records it in ARCHITECTURE §7.

---

## 1. Objective

Board id **P-04**. Three end states:

1. **[ARCHITECTURE.md](../ARCHITECTURE.md) §1, §2.1, §2.2, §3, §3a, §6, §7,
   §8 and §9 describe the shipped tree**, with every number written as a named
   constant and a link to the file that defines it — which is what
   `crates/plurx-core/src/transcode/mod.rs:68` already demands of the rest of
   the codebase: *"Nothing may hardcode the number — the spike measured 4,
   this is now 2, and the property holds for any fixed length. A second copy
   of the value is a failover bug waiting for the day it changes again."* A
   document that quotes `4 s` is that second copy.
2. **A check fails when a quoted constant drifts again.**
   `validation/doc_versions.py` learns the constants the document quotes, and
   `make operations-check` — already the fast lane's `preflight` step
   (`main-fast-lane.yml:97-98`) — gates it.
3. **`docs/README.md`'s status column agrees with the documents it indexes**,
   row by row, each one read before it is changed.

Done means: the extended constant check is green; a grep for each retired
number returns nothing; and the 46 rows listed in §2.2 are each either
corrected or annotated with why they stand.

---

## 2. Contract today

Re-verify at build time. Every row was read in the tree at `0f02b7ea`.

### 2.1 Claim by claim

| # | ARCHITECTURE.md | What the tree says | Verdict |
|---|---|---|---|
| 1 | `:18` "there are no roles to configure" | `cluster_nodes.role` added by migration (`crates/plurx-core/src/cluster/membership.rs:393`), `'learner'` rows at `:2054-2060`, `AUTH_LEARNER_PROTOCOL = 5` (`crates/plurx-core/src/store/hiqlite.rs:171`); OPERATIONS.md:561 "A learner replicates like any other member … but it is not a voter" | wrong |
| 2 | `:40` diagram "Hiqlite: 1 voter (M2) · 3+ future (M3)" | M3 membership shipped; the lab runs three voters and the roster carries learners (`membership.rs:767` `voter_role_persisted`) | stale |
| 3 | `:88` "single-node mode is … a 1-voter raft" | correct — see correction 1. Missing: the one-boot SQLite recovery path (`migration.rs:386-399`) | incomplete |
| 4 | `:100` "Replicated-ephemeral … Raft KV/cache with TTL" | `Cargo.toml:38` builds hiqlite with `features = ["auto-heal", "macros", "sqlite"]`; the `cache` feature exists (`vendor/hiqlite/Cargo.toml:53`) and is **not** enabled. There is no replicated KV tier | wrong |
| 5 | `:157`, `:162` "segment duration `d` (4 s)", "seg=4s" | `SEGMENT_SECONDS = 2` (`crates/plurx-core/src/transcode/mod.rs:72`), `COPY_SEGMENT_SECONDS = 6` (`:99`), `COPY_FIRST_SEGMENT_SECONDS = 2` (`:151`) | wrong |
| 6 | `:244` "a grace window (8 s)" | `ACTOR_HARDWARE_STARTUP_BUDGET = 12 s` (`crates/plurxd/src/transcode.rs:292`), `ACTOR_SOFTWARE_STARTUP_BUDGET = 30 s` (`:295`), `PROGRESS_STALL = 10 s` (`:306`); both HTTP-side values are "compatibility mirrors" owning no timer (`:289-295`) | wrong ×2 |
| 7 | `:300` "Six listed segments" | `LIVE_HLS_OUTPUT_ARGS` (`crates/plurxd/src/live_tv.rs:148-156`) is `-hls_time 1 -hls_list_size 24 -hls_delete_threshold 4`; the comment above it (`:146-147`) reads "The window is 24 entries: 24 s on an encode route and 24 source GOPs on a copy route" | wrong |
| 8 | `:400` "Clients poll — there is no push channel" | no WebSocket or SSE, true. But `POST /api/v1/hls/{session}/control` is a bounded exchange the server may **hold**: `EXCHANGE_DEADLINE = Duration::from_secs(4)` (`crates/plurxd/src/playback_control.rs:25`), advertised cadence 5000 ms, minimum interval 250 ms ([API.md](../API.md) §10). Server-decided prepared switches arrive on a held client request | needs the nuance |
| 9 | `:441`, `:545` "hiqlite 0.14 (spike) → else openraft 0.9 …", "openraft fallback is the same shape" | `Cargo.toml:148-151` is `[patch.crates-io] hiqlite = { path = "vendor/hiqlite" }` and `hiqlite-wal` likewise; both are workspace members (`:11-12`); `vendor/hiqlite/PLURX-PATCH.md` records **fifteen** compatibility patches. The fallback was not taken and the dependency is a fork | wrong |
| 10 | `:449`, `:452-455` "embedded single-file SPA" | `WEB_ASSETS` has 64 rows (`crates/plurxd/src/http/web.rs:57`); `index.html` is a 97-line shell (`:7`). "No build step, no framework" is still true | half wrong |
| 11 | `:533` "No DVR, and no scheduler" | merged `aba14096`, 2026-09-13, PR #294. `DVR_TICK = 15 s` (`crates/plurxd/src/live_tv/dvr.rs:45`), retention sweep every 3600 s (`:52`), settings `dvr.enabled` / `dvr.root` / `dvr.free_floor_gb` / `dvr.tuner_reserve` (`crates/plurx-core/src/store/mod.rs:1502-1505`), `dvr.enabled` defaulting false (`crates/plurxd/src/live_tv.rs:410`) | reversed |
| 12 | `:538` "No transcode-by-default … will not 'optimize' a library into pre-baked renditions" | the pre-transcode pass exists: `DueJob::ProduceCache` → `enqueue_pretranscode_pass` (`crates/plurxd/src/state.rs:5562-5570`, `:5775`), "rank likely titles and put their immutable source generations on the distributed queue", gated on `global.cache_produce_mins > 0` (`:5578`) from `jobs.cache_produce_mins`, whose documented default is off (`crates/plurx-core/src/store/mod.rs:1829-1832`) | true as a default, false as an absolute |

Both DVR documents exist and carry decision headers:
`features/LIVE-TV-DVR-AND-REMINDERS-OPTIONS.md` — *"**Status:** decision
needed · **Written:** 2026-09-13"* (stale; see correction 4) — and
`features/LIVE-TV-DVR-IMPLEMENTATION.md` — *"**Status:** ready to build (v2 —
Astra's review of 2026-09-13 addressed, §10) · **Executes:** the recommended
options in […] §5, which Paul accepted 2026-09-13"*. That second sentence is
the dated decision §3.2 records.

### 2.2 The `docs/README.md` status audit

`docs/README.md:19-23` defines four words — `live`, `open`, `built`, `done` —
and adds the tie-break: *"when a row and a doc's own `**Status:**` header
disagree, the doc wins."* Rows also use a fifth, `superseded`. Status words in
a row are bare; a document's header is free prose.

A word-for-word comparison is useless on free prose, so the audit script
(§3.3, run for this document, output below) reports only a **contradiction**:
a row says `open` while the header's first clause is terminal with no
in-flight qualifier, or a row says `built`/`done`/`superseded` while the
header's first clause is in-flight with no terminal word. Everything else is
printed for a human and never failed.

At `0f02b7ea`: **280** rows carry a status column, **0** point at a missing
file, **35** index documents with no `**Status:**` header at all, **46**
contradictions, **58** unclear. The review's "44 docs marked open whose own
header says merged or complete" is close in size but one-directional; the
real set runs both ways — 27 rows say `open` over a terminal header, 11 say
`built` over "ready to build", 6 say `superseded` over a header that records
no supersession, 2 say `done` over unfinished work.

| README line | Row | Document | Header, first clause |
|---|---|---|---|
| 136 | built | `playback-control/M5-CLIENT-ACTION-OWNERSHIP-HANDOFF.md` | ready to build |
| 143 | open | `playback-control/M5.5-STAGED-GENERATIONS-HANDOFF.md` | the store half is built |
| 144 | superseded | `playback-control/M6-CALLER-HANDOFF.md` | ready to build |
| 145 | superseded | `playback-control/M6-IMPLEMENTATION-HANDOFF.md` | ready to build |
| 146 | done | `playback-control/M6-AXIS-CASE-HANDOFF.md` | ready to run |
| 150 | built | `playback-control/CLIENT-APPLE-HANDOFF.md` | ready to build, message plumbing only |
| 151 | built | `playback-control/CLIENT-ANDROID-HANDOFF.md` | ready to build, message plumbing only |
| 152 | built | `playback-control/CLIENT-WEB-HANDOFF.md` | ready to build, message plumbing only |
| 153 | superseded | `playback-control/M6-APPLE-CLIENT-BUILD.md` | ready to build |
| 154 | superseded | `playback-control/M6-WEB-CLIENT-BUILD.md` | ready to build |
| 155 | superseded | `playback-control/M6-ANDROID-CLIENT-BUILD.md` | ready to build |
| 157 | superseded | `playback-control/M6-APPLE-HARDWARE-ACCEPTANCE.md` | open |
| 159 | built | `playback-control/M6-SERVER-PRIME-HANDOFF.md` | implemented on a branch; validation and merge pending |
| 164 | open | `playback-control/M7-R-M3-CLAUDE-HANDOFF.md` | M1 merged · M2 PR #789 qualifying · M3 not started |
| 208 | open | `streaming/HEVC-SAMPLE-ENTRY-IMPLEMENTATION.md` | ready for implementation; no runtime code built |
| 209 | built | `evidence/hevc-sample-entry-qualification-2026-09-16.md` | focused software evidence green; promotion gate pending |
| 212 | open | `streaming/STREAMING-RELIABILITY-IMPLEMENTATION.md` | six packages and one adversarial review complete |
| 215 | open | `streaming/VOD-PRESENTATION-PLAN.md` | M0 accepted, M1 authorized with amendments |
| 217 | built | `streaming/VOD-PRESENTATION-IMPLEMENTATION-HANDOFF.md` | ready to build |
| 230 | open | `streaming/WEB-HLS-STARTUP-RECOVERY-STATUS.md` | implementation complete · promotion qualified |
| 233 | open | `streaming/WEB-HELD-SEEK-STALL-RCA.md` | implementation, review and qualification complete |
| 238 | open | `streaming/STREAMING-CONTINUATION-HANDOFF.md` | server work landed through 2026-09-08 |
| 239 | open | `streaming/PLAYBACK-CAPS-V2-PLAN.md` | building — M0…M6 merged |
| 242 | built | `streaming/M5A-CLIENT-BADGE-HANDOFF.md` | ready to build |
| 247 | open | `streaming/ANDROID-DV-CONVERSION-IMPLEMENTATION.md` | implementation complete; promotion retry after runner OOM |
| 252 | open | `DECODER_SELECTION_RECOVERY_STATUS.md` | M0–M7 implementation complete and merged |
| 280 | open | `cluster/CLUSTERING-PLAN.md` | executing — M0 through M3 are complete |
| 337 | open | `clients/PLAYBACK-SURFACE-ANDROID-BUILD-PROMPT.md` | §§1–3 and 6 done 2026-09-13 |
| 352 | open | `clients/APPLE-PGS-OVERLAY-ACCEPTANCE.md` | ready for operator execution after build 58 is merged |
| 360 | open | `clients/WEB_LAYOUT_CONTAINMENT_STATUS.md` | live delivery state is recorded on authoritative … |
| 362 | open | `clients/EBOOK-READER-PLAN.md` | M0–M5 complete · M4 device acceptance pending |
| 404 | open | `ci/RIPWIRE-PILOT.md` | trial complete with measurement limits |
| 428 | open | `features/SHOW-IDENTITY-SPLIT-RCA-AND-FIX.md` | M1, M2 and M3 built and … |
| 429 | open | `features/SCAN-IDENTITY-IMPLEMENTATION.md` | M1 merged to the effort; M2 built and under focused … |
| 432 | open | `features/HDHOMERUN-LIVE-TV-PLAN.md` | M0, M1, M2 and M4 Apple merged to the effort |
| 435 | built | `features/LIVE-TV-DVR-AND-REMINDERS-OPTIONS.md` | decision needed |
| 437 | built | `features/LIVE-TV-DVR-IMPLEMENTATION.md` | ready to build (v2) |
| 439 | open | `features/LIVE-TV-DVR-VISIBILITY-STATUS.md` | foundation merged; web follow-up built and reviewed |
| 442 | open | `features/LIVE-TV-GUIDE-AND-UI-PLAN.md` | built baseline; native TV rules superseded by … |
| 447 | open | `features/LIBRARY-CHANNELS-STATUS.md` | shipped on `main`; collection-route correction merged |
| 450 | open | `features/LIVE-TV-NATIVE-LAYOUTS-STATUS.md` | proportions shipped on both native clients |
| 452 | open | `features/LIVE-TV-NATIVE-LAYOUTS-PROPORTIONS-REVIEW.md` | review + fix spec — built and merged 2026-09-12 |
| 453 | open | `features/LIVE-TV-PROPORTIONS-IMPLEMENTATION.md` | executed and merged 2026-09-12 |
| 454 | open | `features/LIVE-TV-GUIDE-AND-START-RELIABILITY.md` | diagnosis complete, fix ruled on by Paul 2026-09-13 |
| 457 | done | `features/LIVE-TV-START-STALL-AND-TVOS-PLAYBACK-SURFACE.md` | diagnosis + design, awaiting Paul's two rulings |
| 462 | built | `features/INTEGRATION-PLAN.md` | ready to build |

The 35 documents with no header are concentrated in `clients/` (9),
`playback-control/` (7), `streaming/` (6) and `apple-builds/` (4); the script
prints them.

**What this list is not.** It is 46 *disagreements*, not 46 *errors*. The
assessment is explicit: a merged pull request does not mean an effort is
accepted, so `open` over "M1 merged" may be the honest row and the header the
stale one. §3.4 resolves each individually and says which way it went.

### 2.3 Where a check can run

`validation/doc_versions.py` (182 lines) already compares documented mobile
build claims against `project.yml` and `build.gradle.kts`, with an explicit
opt-out marker parsed by `HTMLParser` so a historical mention is exempt only
when it is unambiguously marked. `tests/operations/test_mobile_build_claims.py`
and `test_apple_build_claims.py` call it, so it runs inside
`make operations-check`, which runs in the fast lane's `preflight` job on
every non-draft PR. That is the gate this plan extends — no new workflow, no
new trigger.

---

## 3. Change

### 3.1 One PR rewriting §1, §2.1, §2.2, §3, §3a, §6, §7, §8, §9

Every number becomes a named constant with a link to its definition. The rule
the rewrite follows, stated once at the top of §2.3 and obeyed everywhere:
*a number in this document is the name of a constant and a link to the file
that defines it; if you find a bare number here, it is a bug.*

- **§1** — replace "there are no roles to configure" with the truth: every
  node runs the same binary and serves reads, streams and transcodes, and
  membership carries a durable **role** (`cluster_nodes.role`), where a
  learner replicates and relays but holds no vote and may not open a tuner
  (`ARCHITECTURE.md:286` already says the second half). Update the `:40`
  diagram's store line to name voters and learners instead of "1 voter (M2)".
- **§2.1** — keep the 1-voter sentence (correction 1) and add the activation
  boundary: a fresh data directory is imported into the replicated store at
  first boot, activation is one-way, and an interrupted activation yields one
  unreplicated SQLite recovery boot which the log and
  [OPERATIONS.md](../OPERATIONS.md) name. Link
  [CLUSTER-BACKUP-AND-RESTORE.md](../cluster/CLUSTER-BACKUP-AND-RESTORE.md).
- **§2.2** — the **Replicated-ephemeral** row is deleted and its contents
  redistributed. Playback session state is replicated-durable through the
  `Store` trait; node membership/health is replicated-durable in
  `cluster_nodes`. A sentence records that the KV/cache tier was designed and
  not built, that hiqlite's `cache` feature is not enabled, and that building
  it is a separate decision (review §5.3).
- **§3** — the self-repair paragraph is rewritten around the actor:
  `ACTOR_HARDWARE_STARTUP_BUDGET` and `ACTOR_SOFTWARE_STARTUP_BUDGET` bound a
  start, `PROGRESS_STALL` bounds a running producer's output timestamp, the
  prepublication actor commits the verdict, and the HTTP-side values are
  mirrors that own no timer. Link
  [PLAYBACK-CONTROL-PROTOCOL-M4-WATCHDOG-REMOVAL.md](../playback-control/PLAYBACK-CONTROL-PROTOCOL-M4-WATCHDOG-REMOVAL.md).
- **§2.3 failover** — `d` becomes `SEGMENT_SECONDS`, with
  `COPY_SEGMENT_SECONDS` and `COPY_FIRST_SEGMENT_SECONDS` named beside it and
  the ASCII diagram's `seg=4s, keyframes@4s` replaced by
  `seg=SEGMENT_SECONDS, keyframes@N*SEGMENT_SECONDS`. The
  `transcode/mod.rs:68` sentence is quoted in full, because it is the reason
  the rewrite exists.
- **§3a** — "Six listed segments" becomes the live window as it is built:
  `-hls_time 1`, `-hls_list_size 24`, `-hls_delete_threshold 4`, with the
  comment's own reading ("24 s on an encode route, 24 source GOPs on a copy
  route") carried across, and the diagram's `6-segment live window` updated.
- **§5** — "Clients poll — there is no push channel" gains its nuance: there
  is no WebSocket or SSE, and the control exchange is a **bounded long poll**
  the server may hold up to `EXCHANGE_DEADLINE`, which is how a server
  decision reaches a client without a push channel. Link [API.md](../API.md)
  §10.
- **§6** — the Cluster row becomes "hiqlite 0.14, vendored and patched"
  linking `vendor/hiqlite/PLURX-PATCH.md`; the Web app row becomes "embedded
  static app, `WEB_ASSETS` files, no bundler and no framework" linking
  [WEB-SHELL-LAYOUT.md](../clients/WEB-SHELL-LAYOUT.md). The paragraph under
  the table keeps its argument — no npm, no version skew — and drops
  "one hand-written `index.html`".
- **§7** — a new dated decision, per §3.2.
- **§8** — "No DVR, and no scheduler" is **removed as a non-goal** and
  replaced by the boundary that is actually enforced: DVR writes only under
  `dvr.root`, never into a library, off by default. "No transcode-by-default"
  keeps its first sentence and gains the exception: a pre-transcode pass
  exists, is off by default (`jobs.cache_produce_mins = 0`), and never
  outranks a live viewer ([OPERATIONS.md](../OPERATIONS.md):3805).
- **§9** — the hiqlite bus-factor row's mitigation is no longer "openraft
  fallback is the same shape"; it is the vendored fork, its fifteen patches,
  and the `Store` trait. Say that maintaining a fork is now the cost, and
  point at the fork decision (review §4.3), which is not this document's to
  take.

### 3.2 The DVR reversal as a dated decision in §7

A new numbered decision in §7, worded as a reversal rather than as a feature
note, because the thing being explained is why §8 lost a line:

> **9. DVR reverses §8's "no scheduler", decided 2026-09-13.** Recording was
> refused because it introduces a writer with a schedule, a retention policy
> and a conflict resolver. The options were taken deliberately in
> [LIVE-TV-DVR-AND-REMINDERS-OPTIONS.md](../features/LIVE-TV-DVR-AND-REMINDERS-OPTIONS.md)
> and accepted by Paul on 2026-09-13; the constraints that make the writer
> safe are in
> [LIVE-TV-DVR-IMPLEMENTATION.md](../features/LIVE-TV-DVR-IMPLEMENTATION.md) —
> a dedicated `dvr.root` outside every library, a free-space floor, a tuner
> reserve, `dvr.enabled` off by default, and a fixed scheduler cadence.
> §8's read-only rule is unchanged: plurx still never writes into media
> storage.

In the same PR, the options document's own header stops saying "decision
needed" and records the acceptance date (correction 4).

### 3.3 Two checks: quoted constants, and index status

**`validation/doc_versions.py` gains `validate_documented_constants(read)`.**
The document marks each quoted constant in one fixed inline form —
`` `SEGMENT_SECONDS` = 2 `` — and the checker owns a table of
name → (file, pattern, unit):

| Constant | Source | Pattern |
|---|---|---|
| `SEGMENT_SECONDS` | `crates/plurx-core/src/transcode/mod.rs` | `pub const SEGMENT_SECONDS: u32 = (\d+)` |
| `COPY_SEGMENT_SECONDS` | same | same shape |
| `COPY_FIRST_SEGMENT_SECONDS` | same | same shape |
| `ACTOR_HARDWARE_STARTUP_BUDGET` | `crates/plurxd/src/transcode.rs` | `= Duration::from_secs((\d+))` |
| `ACTOR_SOFTWARE_STARTUP_BUDGET` | same | same shape |
| `PROGRESS_STALL` | same | same shape |
| `EXCHANGE_DEADLINE` | `crates/plurxd/src/playback_control.rs` | same shape |
| `DVR_TICK` | `crates/plurxd/src/live_tv/dvr.rs` | same shape |
| `LIVE_HLS_OUTPUT_ARGS` window | `crates/plurxd/src/live_tv.rs` | the value after `"-hls_list_size"` |
| `LIVE_HLS_OUTPUT_ARGS` segment | same | the value after `"-hls_time"` |
| `WEB_ASSETS` | `crates/plurxd/src/http/web.rs` | row count of the `WEB_ASSETS` slice |

Three rules, all failing closed, following the module's existing habit of
refusing anything ambiguous:

1. every name in the table must appear at least once in
   [ARCHITECTURE.md](../ARCHITECTURE.md) — deleting a sentence fails;
2. every occurrence must carry the value the source defines — drift fails,
   in either direction;
3. a `` `NAME` = <number> `` pair whose name is not in the table fails with
   "this document quotes a constant the checker does not know", so adding a
   number to the document without adding it to the table is a build break, not
   a silent exemption.

A new `tests/operations/test_architecture_constants.py` calls it, which puts
it in `make operations-check` and therefore in the fast lane.

**`validation/doc_status_audit.py`** is §2.2's script, landed as-is. It is
reported, not failed: its unclear bucket needs a human, and failing a PR on
free prose would end with authors writing prose to please a regex. It gains a
`--json` mode so the numbers in §2.2 can be refreshed by command rather than
by hand.

### 3.4 The index rows, one at a time

For each of §2.2's 46 rows, in one PR per folder:

1. read the document;
2. if the header states the current fact, **change the row** to the matching
   word (the index's own tie-break rule);
3. if the header is stale, **change the header**, with a date, and leave the
   row;
4. if a merged pull request does not close the effort — the assessment's
   point, and `features/SCAN-IDENTITY-IMPLEMENTATION.md`'s "M1 merged to the
   effort" is exactly this shape — leave `open` and make the row's **Answers**
   cell say what is outstanding, so the next reader does not re-open the
   question.

Two are already decided by §3.2: `features/LIVE-TV-DVR-AND-REMINDERS-OPTIONS.md`
(line 435) takes case 3, and `features/LIVE-TV-DVR-IMPLEMENTATION.md`
(line 437) takes case 3 as well — it is built and merged, not "ready to
build". The 35 headerless documents get a `**Status:**` line in the same
sweep, taken from their own last dated section; a document whose state cannot
be established from its own text gets `**Status:** unknown — last touched
<date>` rather than a guess.

Separately, the three source comments that say "sixty-two"
(`crates/plurxd/src/http/web.rs:8`, `tests/web/asset-graph.js:5`,
`tests/web/shell-source.js:7`) become "sixty-four", which check rule 1 above
would not catch because they are not in ARCHITECTURE.md — so the constant
table's `WEB_ASSETS` row is extended to sweep those three files for a spelt
number too.

---

## 4. Guardrails (non-goals)

- **Do not change a constant in `crates/` to match the document.** The tree is
  the truth; if a constant looks wrong, that is a finding for its own plan.
- **Do not delete §8's non-goals wholesale.** Only the DVR line goes, and only
  because it was reversed by a dated decision with two documents behind it.
  "No external database", "no cloud dependency", "media is read-only" and
  "no linking ffmpeg" are unchanged and load-bearing.
- **Do not present the DVR as an undecided question.** Assessment row
  F-ltv-10: it shipped, the constraints exist, and §7 records the reversal.
- **Do not claim `docs/` has missing documents.** At this commit the index
  test's inventory is satisfied; the unlisted files sit in the five
  directories `tests/operations/test_docs_index.py:24` exempts by policy
  (`apple-builds`, `evidence`, `img`, `mockups`, `archive/retro-2026-08-09`).
  §4.7's "51 unindexed files" was already withdrawn.
- **Do not change an index row without reading the document.** A merged pull
  request is not an accepted effort (assessment rows 4.7 and F-hist-10).
- **Do not make the status audit a failing gate.** §3.3 says why. The
  constant check is the gate; the status audit is a report.
- **Do not enforce document numbers by grep alone.** F-hist-10 asks for
  authoritative constants; the acceptance greps in §5 are a cross-check that
  the old spellings are gone, not the mechanism.
- **Do not touch `docs/README.md`'s folder rules or its three-tier
  structure.** This plan changes status words and one Answers cell per
  corrected row.

---

## 5. Milestones

One PR per milestone, draft into `main` under the fast lane, except M3 which
is one PR per folder.

### 5.1 M1 — the checks, before the prose

`validate_documented_constants` and its table per §3.3,
`tests/operations/test_architecture_constants.py`, and
`validation/doc_status_audit.py` with `--json`. The constant check is wired in
**expecting today's document to fail**, so the PR lands it with the table
populated and the ARCHITECTURE assertions limited to the constants the
document already spells correctly; M2 turns on the rest.

Acceptance: `python3 -m unittest tests.operations.test_architecture_constants`
green; editing `SEGMENT_SECONDS` to `3` in a scratch copy makes it fail naming
the file and both values; `python3 -m validation.doc_status_audit --json | jq
'.contradictions | length'` prints 46 on this tree;
`make operations-check` green.

### 5.2 M2 — the ARCHITECTURE rewrite

§3.1 and §3.2 in one PR, plus the options document's header. Every rewritten
number uses the marked form, and the constant table's rule 1 is switched on
for all eleven entries in the same commit.

Acceptance, all four:

```sh
make operations-check                       # includes the constant check
grep -n 'grace window (8 s)\|seg=4s\|(4 s)' docs/ARCHITECTURE.md      # no output
grep -n 'Six listed segments\|single-file SPA' docs/ARCHITECTURE.md   # no output
grep -n 'no roles to configure\|No DVR' docs/ARCHITECTURE.md          # no output
grep -n 'Replicated-ephemeral\|6-segment live window' docs/ARCHITECTURE.md  # no output
python3 -m unittest tests.operations.test_docs_index                  # green
```

and a fifth that is an observable fact rather than a command: §7 carries a
numbered, dated decision naming both DVR documents, and §8 no longer contains
a non-goal the tree contradicts.

### 5.3 M3 — the index rows and headers

§3.4, one PR per folder (`playback-control/`, `streaming/`, `features/`,
`clients/`, `cluster/`, `ci/`, `evidence/`, plus the two top-level rows). Each
PR's body lists, per row, which of the four cases applied and the sentence in
the document that decided it.

Acceptance: `python3 -m validation.doc_status_audit` reports zero
contradictions for the folders that PR covers and no new ones elsewhere;
`make operations-check` green (`test_docs_index` and `test_status_pr_claims`
both run there); every document that PR touched has a `**Status:**` header.

### 5.4 M4 — the "sixty-two" comments

`crates/plurxd/src/http/web.rs:8`, `tests/web/asset-graph.js:5`,
`tests/web/shell-source.js:7`, plus the `WEB_ASSETS` sweep extension from
§3.4's last paragraph.

Acceptance: `grep -rn 'sixty-two' crates/ tests/ docs/` returns nothing;
`make operations-check` green; adding a 65th `WEB_ASSETS` row in a scratch
copy makes the constant check fail naming all four files.

---

## 6. Verification and rollout

Python gate for every milestone: `make operations-check` plus `python3 -m
unittest discover -s tests/validation -p 'test_*.py'`. Node gate for M4:
`node tests/web/asset-graph.js` and `node tests/web/shell-source.js`, because
both carry the comment being changed. No Rust, Swift or Kotlin changes
anywhere in this plan, so no compile surface moves and
`validation.ci_scope` should select the documentation-only lane for M2 and M3
— which is itself worth checking, since M1 and M4 touch `validation/`,
`tests/` and `crates/` and must not.

Nothing here needs a device or the fleet. There is no GPT prompt for this
plan; every claim in §2.1 was settled by reading the tree, and the one thing
that could not be — whether the lab actually runs a learner today — does not
change what §1 must say, because the role exists in the schema either way.

Rollout order: M1 → M2 → M3 → M4. M1 first is the point: a document repaired
under a check stays repaired, and a document repaired without one is this
plan again in November. M3 and M4 may run in parallel with each other once M2
has merged; neither may run before M1, because both rely on the audit script
and the constant table.

---

## 7. Open questions

1. **Whether the `Replicated-ephemeral` tier is deleted or deferred.** §3.1
   deletes the table row and records the tier as designed-not-built, which
   matches review §5.3 ("corrected in the architecture now and designed, if at
   all, later"). If Paul wants it built, the row stays with a dated "not yet"
   and this plan does not change.
2. **How §9 should describe the fork's cost.** This document states that
   maintaining fifteen patches is now the mitigation; the actual fork decision
   — own the stack, upstream the generic fixes, or both — is review §7.2 and
   belongs to the hiqlite plan, which is not yet written. §3.1 writes the fact
   and links `vendor/hiqlite/PLURX-PATCH.md`, not the decision.
3. **Whether a `**Status:**` header should be mandatory.** M3 gives 35
   documents one. Making it a gate (every document indexed with a status
   column carries a header) is a small operations test and a real policy
   change; it is not in this plan because it would fail on documents nobody is
   about to read.
4. **What `open` means for an effort whose implementation merged.** The four
   cases in §3.4 encode an answer — merged ≠ accepted — and it is worth Paul
   confirming it once, because 27 of the 46 rows turn on it.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| | | | | | |
