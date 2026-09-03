# Fragment-index queue repair — every build since 2026-08-31 died at its first heartbeat

**Status:** ready to build · **Root cause:** established and execution-proven
(§1) · **Answers:** `FRAGMENT-INDEX-HEALTH-FINDINGS.md` and the companion
`DV-P7-CONVERSION-NOT-REACHING-PLAYBACK.md` (both in `~/code/plurx-agent/`)
· **Verified against:** `origin/main` @ `1635c1a9` and the live fleet, read on
m6 at 2026-09-03 02:06 UTC · **Reviewed:** two adversarial reviews, 2026-09-03;
21 corrections folded in · **Written:** 2026-09-03

Companion to [CONTENT-ANALYSIS-INDEX-HANDOFF.md](CONTENT-ANALYSIS-INDEX-HANDOFF.md)
(how the analysis request queue was designed, and the rules this plan must
not break) and [PLAYBACK-CAPS-V2-PLAN.md](PLAYBACK-CAPS-V2-PLAN.md) §4.7 (why
one file has several index identities). This document is *why the queue has
produced nothing for three days, and how to make it produce again* — in that
order, because the second half is small once the first half is understood.

Read §1 first; it settles the "cause not established" line in the findings
document. Then §5: M0 is a two-statement fix that turns the queue back on
and ships alone; M1–M5 exist so this class of defect cannot ship silently a
second time — and it has shipped before (§1.4). If a step seems to require
changing the request-queue design in CONTENT-ANALYSIS-INDEX-HANDOFF.md
(§5.3 forced generations, §5.4 exact fences, §6.8 no in-place reapers) or
the identity model in PLAYBACK-CAPS-V2-PLAN.md, stop and flag it — §8 lists
the rulings Paul has to make before that kind of change.

---

## 1. What is broken — the heartbeat statement never reaches the store

### 1.1 The underlying error behind all 1,933 `attempt_limit` rows

The findings document could not name the error because the row keeps only
`last_error_code`, and by the fifth attempt that reads `attempt_limit`. The
error is recoverable from two other places, and both say the same thing.

**The replicated lifecycle counters** (`analysis_lifecycle_counters`, exported
as `plurx_analysis_lifecycle_total`), cumulative since the schema landed:

| event | reason | count |
|---|---|---|
| claim | all | 12,193 |
| lease_loss | lease_expired | **9,915** |
| retry | lease_expired | 7,978 |
| retry | source_attestation_timeout | 160 |
| retry | foreground_preempted | 26 |
| failure | attempt_limit | 3,880 |
| failure | other | 202 |

81 % of every claim ever made ended as a lost lease. Not an ffmpeg error, not
a NAS error, not a bad file — the worker stopped renewing.

**The source** says why. `renew_cluster_fragment_index` on the replicated
store (`crates/plurx-core/src/store/hiqlite_fragment_index_cluster.rs:2868`)
binds its parameters as `$1 … $6` and then writes the `WHERE` clause with
`target_node_id = $6` *before* `owner_node_id = $4 AND fence = $5`:

```sql
UPDATE cluster_fragment_index_jobs
   SET lease_expires_ms = $1, updated_at_ms = $2
 WHERE cache_key = $3 AND target_node_id = $6          -- $6 appears before $4
   AND state = 'running' AND owner_node_id = $4
   AND fence = $5 AND lease_expires_ms > $2 AND $1 > $2
```

Every replicated statement passes through `validate_sql`
(`crates/plurx-core/src/store/hiqlite.rs:3184`), called by both
`TimedClient::execute`/`txn` (`:1045`, `:1148`) and `HiqliteAuthStore::execute`
(`:2441`) before any I/O. Its `validate_parameter_order` (`:3195`) requires
placeholders to *first appear* in ascending order — because hiqlite binds by
rusqlite's numeric index while SQLite treats `$N` as a named parameter
indexed by first appearance, so an out-of-order statement would bind the
wrong values silently. The validator refuses the statement before it is
sent:

```
hiqlite placeholders must first appear in order; expected $4, found $6
```

`yield_cluster_fragment_index` (`:2897`) has the identical defect. Both were
introduced by `ce253a55` (2026-08-31, "harden queue lifecycle and client
markers", PR #700 via `d4ddf28e`) when `target_node_id` joined the primary
key and was appended to the existing `WHERE` clause as `$6`. The SQLite twin
(`sqlite/fragment_index_cluster.rs:2104`, `:2136`) uses rusqlite `?N`
positional binding, which is by number rather than by appearance, so the
embedded backend is functionally correct — which is why no SQLite-only test
noticed.

**Execution proof** (run in the cloud container, 2026-09-03, on `1635c1a9`
with a scratch test that slices the two literals out of the source and feeds
them to `validate_sql`; independently re-run by a reviewer with the same
result):

```
fn renew_cluster_fragment_index: database error: hiqlite placeholders must first appear in order; expected $4, found $6
fn yield_cluster_fragment_index: database error: hiqlite placeholders must first appear in order; expected $4, found $6
test ... fiq_proof::renew_and_yield_statements_are_rejected_by_the_placeholder_validator ... ok
```

No test in the tree calls either method through any store backend
(`grep -rn renew_cluster_fragment_index crates` finds only the two store
implementations, the trait, and the daemon call site), so nothing could have
failed.

### 1.2 What the daemon does with a heartbeat that always errors

`run_cluster_fragment_index_job` (`crates/plurxd/src/state.rs:6393`) spawns
the heartbeat before it does anything else (`:6421-6456`):

```rust
let mut interval = tokio::time::interval(renew_every);   // lease_ms / 3 = 20 s
loop {
    tokio::select! {
        _ = stop.cancelled() => break,
        _ = interval.tick() => {
            match store.renew_cluster_fragment_index(...).await {
                Ok(true) => {}
                Ok(false) | Err(_) => { lost.cancel(); break; }   // no log line
            }
        }
    }
}
```

`tokio::time::interval`'s first tick completes immediately (the pretranscode
heartbeat at `:1825` consumes that tick deliberately; this one does not), so
the renew runs within microseconds of the claim, the validator rejects it,
and `lost` is cancelled before the worker has read the catalog row. Nothing
between the spawn and the build checks `lost`, so the worker still does the
catalog read, the probe read, `inspect_source`, and — on a memo miss — the
full-file SHA-256 attestation (up to `ATTEST_TIMEOUT` = 10 min) on a lease
it no longer holds. A job that fails in any of those steps writes its real
code through `fail_cluster_fragment_index`, whose statement is valid; that is
where the 160 `source_attestation_timeout` retries in §1.1 come from. A job
that gets as far as the build `select!` (`:6678-6701`) hits
`() = lost.cancelled() => (None, false)` (`:6696`), and the `None` outcome
returns without writing anything (`:6716-6719`) — the only return path in
the function that leaves the row untouched.

So every claimed job that reaches the build step, on every voter, since the
2026-08-31 deploy:

```
 claim ─▶ row = running, lease = now + 60 s, attempts += 1
   │
   │ (first heartbeat tick, t ≈ 0)   renew → Err(validator)
   │                                  lost.cancel()
   ▼
 worker reaches the build select!, returns, writes NOTHING
   │                                          row stays `running`
   │ ≤ 60 s + the next CLAIM sweep on any node
   ▼
 sweep:  attempts < 5 ──▶ queued, last_error_code = lease_expired, backoff
         attempts ≥ 5 ──▶ failed, last_error_code = attempt_limit
```

Five laps of (60 s lease + 5 · 2ⁿ s backoff + up to 60 s tick latency) is
6–10 minutes, and the row-lifetime histogram of the 1,933 dead jobs peaks
exactly there: 8 min 136 rows · 9 min 227 · 10 min 171 · 11 min 113, with a
long tail from waiting for one of the two cluster-wide slots. The `yield`
paths (`node_local_refusal`, uncharged) error the same way, so a preempted
job is also left `running` and charged an attempt it should not have paid.

### 1.3 The numbers, reconciled with the findings document

All from the replicated state machine on m6, 02:06 UTC. They differ from the
01:52 read by the fifteen minutes between them.

| Fact | Value |
|---|---|
| Artifacts ever built | 932 (851 distinct files of 5,847) |
| **Last artifact built** | **2026-08-31 19:xx UTC** — zero since |
| Build rate before that | 12–16 / hour fleet-wide (277 on 08-30, 233 on 08-31 up to 19:00) |
| Jobs `failed / attempt_limit` | 1,933, every one at `attempts = 5` |
| Distinct `cache_key`s among them | 617 — 363 files carry a dead row for **all four** target nodes |
| Requests `failed / attempt_limit` | 1,956; 26 of them died at the request level and never had a job row |
| First `attempt_limit` row | 2026-08-31 19:xx UTC, the same hour the last artifact was built |
| Peak | 2026-09-01, 1,294 rows: the queue draining the backlog at full speed into a wall |
| `failed / queue_expired` | 109 — jobs targeted to a node that could not win a slot for 6 h (8 of the queued requests are targeted at nuc3, a learner, which never drains) |
| Unresolved background requests | 639 `queued`, `attempts = 0`, no cache key yet: the attestation backlog (154 nynuc · 231 nuc4 · 250 m6) |
| `running` rows with an expired lease | 11 (8 on nynuc, 3 on m6), each claimed within two seconds of its neighbours |
| Targeted jobs that ever succeeded | 8, all dated 08-26…08-29, before `ce253a55` |
| Node clocks | NTP-synchronised, within 0.3 s of each other — skew is ruled out |
| Store writes on the current process (m6) | 4,177 ok · 0 error · 0 cancelled — the store is healthy; the statement never reaches it |

The eleven stale `running` rows are the mechanism caught in the act: a slot
claims a job, the job returns in ~100 ms without writing, the slot claims the
next, four times per slot, two slots — eight rows on nynuc claimed between
02:01:58 and 02:02:00. They sit past their lease until any node's next claim
sweep, which is delayed on a busy node because the sweep only runs inside
`work_cluster_fragment_index_queue` *after* `resolve_analysis_requests` has
finished hashing up to two files under a ten-minute timeout each.

The 09-01 spike is therefore not an event on 09-01. It is the deploy of
`ce253a55` late on 08-31 (the artifact clock stops and the failure clock
starts in the same hour, 19:00 UTC) followed by a full day of the queue
retrying a ~600-key backlog at its maximum rate.

### 1.4 Why nothing said so, and why this is the second time

- The heartbeat swallows both `Ok(false)` and `Err(_)` without a log line.
  The request-level heartbeat (`state.rs:5844-5867`, `renew_analysis_request`)
  has the identical shape; its statement happens to be valid today.
- The eleven `fail_cluster_fragment_index` and six `yield_cluster_fragment_index`
  results in the runner are discarded (`let _ = …`), so a write that matched
  zero rows — or errored — is indistinguishable from success. Only
  `complete` (`:6930-6945`) inspects its result, and only for `Err`.
- `last_error_code` is set to `NULL` on every claim and overwritten by the
  terminal `attempt_limit`, so the row itself carries no history.
- The per-code retry counters that *do* record it are aggregate only, and no
  surface reads them: `plurx_analysis_lifecycle_total` is scraped by nothing,
  the per-process `plurx_analysis_artifact_publications_total` (`state.rs:1266`)
  resets on every restart, and the Settings → Analysis view lists jobs but
  has no "artifacts built in the last 24 h" figure, so a queue that fails
  100 % of the time looks like a queue with a lot of history.
- Playback degrades to live-HLS recovery on a missing index, which plays.
- **This defect class has four ledger rows already**: `4da3bbde`
  ("positional Hiqlite bindings corrupt publication sentinels, cursor limits,
  lease resources, or route identity"), `c2d0725b`, `43c90879`, `35352d40`.
  The fix for the first added
  `every_replicated_placeholder_is_introduced_in_order`
  (`hiqlite_sessions.rs:3638`) — a census of exactly the right rule, scoped
  to one module. Six weeks later the same mistake landed in two other
  modules. M0.3 is that test, repo-wide.

The findings document's §3 is right that it is invisible; §5 Q3's "there is
no counter" is not quite right — `plurx_analysis_queue_depth{state="failed"}`
and the lifecycle counters exist and were, in fact, the evidence. What does
not exist is anything that turns "12,193 claims, 9,915 lease losses, 0
artifacts in 72 h" into a sentence an operator sees. M3 is that.

### 1.5 A sibling defect in the same class, found by the same census

Running the placeholder-order rule over every production string literal in
`crates/plurx-core/src/store/hiqlite_*.rs` finds exactly **six** statements it
rejects. Two are the ones above. The other four are the `dv_conversions`
ledger writes in `hiqlite_publication.rs` (`:1331` running, `:1372` verified,
`:1514` committed, `:1766` failed — all `SET … = $2 … WHERE file_id = $1`),
added by `c59e4a45` (2026-08-31, M5b on-disk P7→8.1 conversion). They go
through `atomic_publication` → `txn` (`hiqlite_publication.rs:170`), which
validates. On the replicated store the M5b conversion cannot advance a
`dv_conversions` row past `queued`. It is out of this document's scope but in
M0's blast radius: the census test in M0.3 will refuse to pass until those
four are reordered too, so reorder them in the same PR (a parameter
renumbering, not a behaviour change — §3.1 has the one constraint) and say
so in the PR body for whoever owns M5b.

---

## 2. How the pipeline works today — only what the fix touches

The full design is in CONTENT-ANALYSIS-INDEX-HANDOFF.md. What matters here:

```
 discover (per voter, every vod_index_mins = 15 min, 4 files / pass,
           cursor-ordered, node-targeted)                  state.rs:5585
     │  request_file_analysis(file, background)            state.rs:3095
     ▼
 analysis_requests  (dedup key includes target_node_id → one row per voter)
     │  resolve: stat + FULL-FILE SHA-256 (memo'd) ≤ 10 min, 2 / tick   state.rs:5795
     ▼
 cluster_fragment_index_jobs  PK (cache_key, target_node_id), queued
     │  drain: 2 cluster-wide slots `media:fragment-index:{0,1}`,
     │         4 jobs / slot / tick, sequential                state.rs:5708
     ▼
 claim (fence+1, attempts+1, lease 60 s)  ──▶  run_cluster_fragment_index_job
        heartbeat every 20 s ───────────────▶  renew  ◀── BROKEN (§1)
        build ≤ index_file_budget ──────────▶  complete → artifact + location
```

Facts the milestones rely on (all at `1635c1a9`):

- `DEFAULT_ANALYSIS_MAX_ATTEMPTS = 5`, lease 60 s, backoff 5 s · 2ⁿ capped
  at 300 s (`fragment_index_cluster.rs:521-528`); settings
  `analysis.max_attempts / lease_secs / backoff_base_secs / backoff_max_secs`.
- The claim wipes `last_error_code` (`hiqlite_fragment_index_cluster.rs:2755`).
  On a **retryable** failure `fail` writes the caller's code while
  `attempts < max` and `attempt_limit` once `attempts ≥ max` (`:3204-3208`);
  a non-retryable failure (`unsupported`, `truncated`, `source_superseded`,
  `pipeline_superseded`, `source_changed`, `encode_failed`) always writes the
  caller's code and goes straight to `failed`.
- The **claim** sweep (`:2682-2711`) is the only writer of `lease_expired`:
  a `running` row past its lease goes back to `queued` with backoff when
  `attempts < max`. The claim, enqueue (`:2573-2584`) and requeue
  (`:2800-2811`) sweeps all write `attempt_limit` for a lapsed `running` row
  with `attempts ≥ max`, and `queue_expired` for a `queued` row older than
  `QUEUE_ELIGIBILITY_MS` = 6 h.
- **No `failed` job is ever re-requested by discovery, whatever its code.**
  `enqueue_analysis_request` refuses a normal request when any row with the
  same `(file, size, mtime, component, pipeline_version, requested_generation,
  target_node_id)` exists in any state (`:1061-1064`); the background
  generation is a pure function of those fields (`state.rs:3122-3131`); and
  `prune_analysis_requests` keeps the newest normal terminal row per identity
  (`:1933-1939`). So a `queue_expired` job is as dead as an `attempt_limit`
  one unless playback's foreground `enqueue` (`vodserve.rs:1851-1887`, which
  reopens only `cancelled`/`queue_expired`, `:2628-2634`) or an admin request
  touches it. `POST /api/v1/files/{id}/analysis` auto-escalates to
  `force=true` when it joins a failed request (`http/analysis.rs:289-293`);
  `POST /api/v1/analysis/jobs/{request_id}/retry` (`:667-698`) calls
  `retry_analysis_request_admin` (`hiqlite_fragment_index_cluster.rs:1992`),
  which inserts a **successor** request with a fresh forced generation and
  never mutates the failed row — that is the design's only reopen primitive.
- `analysis_requests_one_active_source` is a UNIQUE index over the identity
  for active states, and `analysis_requests_one_active_forced_successor`
  admits one active forced request per `(file, size, mtime, component)`
  (`fragment_index_cluster.rs:285-291`).
- `analysis_lifecycle_counters` is maintained by SQL triggers on the jobs
  table (`fragment_index_cluster.rs:467-517`, hiqlite twin `:529-579`) and
  is the only durable per-code retry record. Requests keep per-attempt rows
  in `analysis_attempts` (keyed by request fence, ≤ 64 per request); jobs
  keep nothing.
- `/metrics` must not touch the store: `prometheus_scrape_has_no_store_operation`
  (`http/system.rs:4087`) pins it. Store-derived gauges are sampled by
  `store_metrics_loop` (`state.rs:735`) into `prometheus_store_snapshot`.
- The web UI already has an Analysis view (`#/analysis`, `viewAnalysis` at
  `index.html:10993`) with per-row retry/cancel and a code → operator-text
  table (`:11090-11113`). The `attempt_limit` entry there says "Resolve the
  underlying error shown in earlier attempts" — text that assumes a history
  the job row does not keep.

---

## 3. Contract — exact statements and signatures

Re-verify every line number against the file at build time; `main` moves
several times a day. The *content* is what is binding.

### 3.1 The two statements, corrected (M0)

`crates/plurx-core/src/store/hiqlite_fragment_index_cluster.rs`, replace the
SQL text only — the `params!(...)` order and the Rust signature stay:

```sql
-- renew_cluster_fragment_index (was :2879)
UPDATE cluster_fragment_index_jobs
   SET lease_expires_ms = $1, updated_at_ms = $2
 WHERE cache_key = $3 AND owner_node_id = $4 AND fence = $5
   AND target_node_id = $6
   AND state = 'running' AND lease_expires_ms > $2 AND $1 > $2
```

```sql
-- yield_cluster_fragment_index (was :2908)
UPDATE cluster_fragment_index_jobs
   SET state = 'queued', owner_node_id = NULL, lease_expires_ms = NULL,
       attempts = MAX(attempts - 1, 0), not_before_ms = $1,
       last_error_code = 'node_local_refusal', updated_at_ms = $2
 WHERE cache_key = $3 AND owner_node_id = $4 AND fence = $5
   AND target_node_id = $6
   AND state = 'running' AND lease_expires_ms > $2
```

The predicate set is unchanged; only the clause order moved so that `$4`,
`$5`, `$6` first appear in that order. Do the same reordering in the SQLite
twin (`sqlite/fragment_index_cluster.rs:2104`, `:2136`) so the two backends
read identically — the parity test compares schema, not statements, so this
is discipline rather than a gate.

The four `dv_conversions` statements in `hiqlite_publication.rs` (§1.5) are
the other shape of the same mistake — `SET … = $2 … WHERE file_id = $1`.
Renumber each so the `SET` values are `$1…` and `file_id` takes the next
ordinal, and reorder the matching `params!` the same way. **One constraint:**
`bind_atomic_authority` (`hiqlite_publication.rs:66-102`) locates the five
lease parameters (`resource, owner_node_id, fence, revision, expires_at_ms`)
by contiguous value match, so they must stay contiguous and in that order
after the renumbering. Check each against the M0.3 census before opening
the PR.

### 3.2 The census rule (M0.3)

Generalise `every_replicated_placeholder_is_introduced_in_order`
(`hiqlite_sessions.rs:3638`) from one module to every `hiqlite_*.rs` store
file. It is a unit test inside `plurx-core` because `validate_sql` is
`pub(super)`:

```rust
// Every replicated statement in these files must pass the validator the
// store applies at runtime. A statement that fails here is refused silently
// in production — see FRAGMENT-INDEX-QUEUE-REPAIR-HANDOFF.md §1.
for (file, source) in STORE_SOURCES {                       // include_str! each
    for literal in production_sql_literals(source) {
        validate_sql(&literal).unwrap_or_else(|error| panic!("{file}: {literal}\n{error}"));
    }
}
```

`production_sql_literals` = every Rust `"…"` literal (with `\"` and `\n`
escapes decoded) that contains `UPDATE`, `INSERT`, `SELECT`, `DELETE`, or
`WITH` as a word *and* a `$`, **outside `#[cfg(test)]` modules**. Strip test
modules by brace-matching from each `#[cfg(test)]`, not by splitting at the
first occurrence: `hiqlite_media.rs` has a test module at `:706` with ~2,500
lines of production statements after it. A naive scan of the checkout hits
25 rejections; six are real (§1.5) and 19 are not:

- test fixtures inside store files: `hiqlite_catalog.rs:343`,
  `hiqlite_media.rs:715, 720, 725`, `hiqlite_sessions.rs:3776, 3863, 3865,
  3925, 3933, 3939, 3941, 3966` — excluded by the test-module strip;
- `format!` fragments and templates: `hiqlite_media.rs:783, 801, 1429, 1447`
  (`GENRE` into the two `items` page templates), `hiqlite_durable.rs:1566,
  1571` (the membership tombstone clause into its host), and
  `hiqlite_import.rs:1688` (`LIMIT ${limit}`) — assemble these seven in the
  test exactly as their call sites do, then validate the result.

No per-line exemption marker: a marker is a loophole a future production
statement can opt into. `disconnected_test_client()` (`hiqlite.rs:1180`)
exists for helper-level validation tests if the census needs a client.
Mutation check: swap the **first appearances** of two placeholders in any
production statement and the test names the file, the statement and the
ordinal it expected (swapping a later repeat is a logic change the validator
cannot and need not see).

### 3.3 Heartbeat and write-result contract (M1)

Adopt the shape `ActivePretranscodeJob::start` already has
(`state.rs:1808-1915`): consume the immediate first tick,
`MissedTickBehavior::Delay`, race each renewal against
`sleep(lease_time_remaining)` so the task self-fences at the deadline
whatever the tick timing, and log every transition. Diverge from it in one
place, deliberately: it treats a renew `Err` as lost (`:1888-1898`); here an
`Err` is retried on the next tick **until the deadline sleep arm fires**,
because a transient store timeout (`STORE_TIMEOUT` = 3 s, `hiqlite.rs:126`)
must not abandon a build that still holds a valid lease. The deadline is a
`select!` arm, not a `clock_ms() <` check sampled on ticks — tick sampling
can overrun the other nodes' sweep by up to a store timeout. Overrun is
wasted work, not corruption: `complete` requires `lease_expires_ms > now`
(`:2971`) and orphaned `install_local_blob` output is swept — but the sweep
is what a fence is for, so do not lean on it.

```rust
Ok(true)             => { known_expiry = requested_expiry; }
Ok(false)            => { warn!(cache_key, target, fence, attempts, "fragment-index lease lost: row no longer ours"); lost.cancel(); break; }
Err(error)           => { warn!(…, %error, "fragment-index lease renew failed; retrying before expiry"); }
() = sleep_until(known_expiry) => { warn!(…, "fragment-index lease expired without a renewal"); lost.cancel(); break; }
```

Factor the loop into a free `run_lease_heartbeat(renew: impl FnMut(i64) ->
Fut<Result<bool, StoreError>>, lease_ms, stop, lost)` and use it for **both**
the job heartbeat and the request heartbeat (`state.rs:5844-5867`, same
silent shape). `Store` is a blanket impl over 24 traits
(`store/mod.rs:2922-2953`), so a stub store is not a test double this repo
can afford; a closure is.

**Stop the heartbeat before the outcome write, not after.** Today
`finish_heartbeat` follows every `fail`/`yield`/`complete`; a tick landing
between the write and `stop.cancel()` gets `Ok(false)` and would, under this
contract, log a false "lease lost".

Every `fail_cluster_fragment_index` / `yield_cluster_fragment_index` /
`complete_cluster_fragment_index` call in the runner — eighteen: eleven
`fail`, six `yield`, one `complete` — goes through one helper
(`record_job_outcome`) that logs at `warn` on `Ok(false)` or `Err` with
`cache_key`, `target_node_id`, `fence`, `attempts`, and the code it was
trying to write. `record_fragment_index_source` is not an outcome and stays
as it is.

### 3.4 Per-attempt error history (M2)

One column, both backends, one schema step:

```sql
ALTER TABLE cluster_fragment_index_jobs
  ADD COLUMN attempt_errors TEXT NOT NULL DEFAULT '';
```

A column rather than rows in `analysis_attempts` because a job can exist
without a request (playback's foreground enqueue) and that table is keyed by
request fence. Semantics: a comma-separated list of the code each
**charged** attempt ended with, oldest first, never cleared by a claim:

| writer | appends |
|---|---|
| `fail_cluster_fragment_index` (every attempt, including the one that becomes `attempt_limit`) | the caller's `error_code` — the real one, not `attempt_limit` |
| claim sweep, `running` → `queued` | `lease_expired` |
| claim / enqueue / requeue sweep, `running` → `failed / attempt_limit` | `lease_expired` |
| `yield_cluster_fragment_index` (uncharged — it refunds the attempt) | **nothing**; a job preempted every tick would otherwise grow the column without bound |
| cancellation (`cluster_fragment_indexes_cancel_source`, `analysis_requests_supersede_source` triggers, `cancel_analysis_request_admin`, `publication_superseded`) | nothing — the row leaves the retry budget |
| `complete_cluster_fragment_index` | nothing — success ends the history |
| every reopen that resets `attempts = 0`: `submit_fragment_index_analysis` (`:1568-1602`), `enqueue_cluster_fragment_index` (`:2612-2627`), `requeue_cluster_fragment_index` (`:2828-2847`), and the forced-generation admin retry | resets to `''` — a fresh budget starts a fresh history |

Append in SQL so the sweeps can do it inside their existing `txn` batches:
`attempt_errors = CASE WHEN attempt_errors = '' THEN $code ELSE attempt_errors || ',' || $code END`.
With yields excluded the list is bounded by `MAX_ANALYSIS_MAX_ATTEMPTS = 20`
entries × `MAX_ERROR_CODE_BYTES = 64`. `last_error_code` keeps its current
meaning (`attempt_limit` stays the terminal code the UI and the lifecycle
triggers key on — the trigger is `AFTER UPDATE OF state` and does not touch
the new column); the new column is the history the UI text already promises.

Surface it: `JOB_COLS` and `JobRow` gain the column; the history CTE
(`ANALYSIS_CANONICAL_CTE`, `fragment_index_cluster.rs:626-744`) carries it
as `job_attempt_errors`; `GET /api/v1/analysis/jobs` rows and
`GET /api/v1/analysis/jobs/{request_id}` include it; the Analysis view shows
it under the code in the row. Diagnosis then is one query:
`SELECT attempt_errors, COUNT(*) FROM cluster_fragment_index_jobs WHERE last_error_code = 'attempt_limit' GROUP BY 1`.

**Migration mechanics — a schema bump is a stop-the-fleet event here, not a
rolling upgrade.** `schema_migration_action` (`hiqlite.rs:3314-3358`) and
`verify_compatibility_rows` (`:3360-3381`) refuse any schema other than the
binary's own, so an old binary that *restarts* after the new schema commits
refuses to open (a running old process keeps working: every read names its
columns via `JOB_COLS` and every INSERT lists columns, and the new column
defaults). OPERATIONS.md "Upgrading an activated cluster" is the procedure:
voters first, the learner (nuc3) last, no old binary restarted afterwards.
Bundle every schema change in this series into the one step. The step
itself, following the v25 pattern:

| where | what |
|---|---|
| `sqlite/mod.rs` `MIGRATIONS` | append as v45 (v44 is the DV recovery witness) |
| `hiqlite.rs:60-74` | `ATTEMPT_ERRORS_SCHEMA_VERSION = 26`, `AUTH_SCHEMA_VERSION` = it |
| `hiqlite.rs:77-96` | the matching `*_SCHEMA_MIGRATION_SOURCE` constant |
| `migrate_schema` (`:1943-1985`) | a `MigrateFrom(<25's source>)` arm: `txn([ALTER …, "UPDATE cluster_meta SET schema_version = $1 …"])` + `settle_migration_attempt` — needed because `ADD COLUMN` is not idempotent across two voters racing the step |
| `schema_migration_action` (`:3330-3350`) | add the source to the accepted list |
| `hiqlite.rs:5040-5060` | the chain assertion `AUTH_SCHEMA_MIGRATION_SOURCE + 20 == AUTH_SCHEMA_VERSION` becomes `+ 21` |
| `install_schema` (`hiqlite_fragment_index_cluster.rs:652-668`) | the fresh-install path; its probe `analysis_component_schema_is_current` (`:649`) counts 15 schema **objects** — a column adds none, so the probe must check the column or a fresh cluster never applies the step |
| `store_contract.rs` | a `replicated_v26_store_migrates_attempt_errors_on_daemon_open` twin of `replicated_v24_…` (`:9091`) |
| parity test (`:3496-3525`) | it compares only the v30/v31/v33 lists today; add the v22/v41 `ANALYSIS_COMPONENT_STATEMENTS` step first, then this one |

No `PROTOCOL_VERSION` bump: nothing on the wire changes.

### 3.5 Queue-health contract (M3)

The jobs table cannot answer "how many claims in 24 h": a row keeps only its
latest transition (`last_error_code` is wiped on claim, `updated_at_ms` is
one timestamp), so a row claimed five times is one row, and only the
cumulative lifecycle counters count events. Define the verdict on what *is*
computable, cheaply, in `store_metrics_loop` — never in the HTTP handler
(`/metrics` may not touch the store, §2):

```json
"health": {
  "ready_24h": 0,                 // jobs state='ready' with updated_at_ms in window
  "attempt_limit_24h": 84,        // jobs state='failed', code attempt_limit, in window
  "running_past_lease": 11,       // state='running' AND lease_expires_ms < now
  "lease_losses_since_start": 398,// this process's delta of the lifecycle counter
  "claims_since_start": 412,      // same, event=claim
  "last_ready_at_ms": 1787000000000,
  "verdict": "dead"               // "healthy" | "degraded" | "dead" | "idle"
}
```

`ready_24h` and `attempt_limit_24h` come from `cluster_fragment_index_jobs`
via `cluster_fragment_index_jobs_status_history(state, updated_at_ms DESC, …)`
(`fragment_index_cluster.rs:215`); `ready` rather than artifact rows because
after M5.1 a hydration completion produces a `ready` job and a location but
no new artifact row (`complete` inserts artifacts `ON CONFLICT DO NOTHING`,
`:2983`) and a healthy hydrating fleet must not read as dead. The two
`since_start` figures are per-process deltas of `analysis_lifecycle_counters`
sampled every loop tick — honest, and they reset on restart, and the field
names say so. Verdict rules, each with its reason:

- `idle` — `claims_since_start` = 0 and `ready_24h` = 0. Nothing to judge;
  the library may be fully indexed or discovery paused (`vod_index_mins = 0`).
- `healthy` — `ready_24h` ≥ 1 and lease losses < 25 % of claims since start.
  A quarter is generous; preemption by foreground playback is a legitimate
  loss.
- `degraded` — `ready_24h` ≥ 1 but losses ≥ 25 %, **or** `attempt_limit_24h`
  ≥ 10 % of claims since start, **or** `running_past_lease` > 0 for two
  consecutive samples. Something is wrong with part of the library, one
  node, or the sweep.
- `dead` — `claims_since_start` ≥ 20 and `ready_24h` = 0. Twenty is enough
  claims that "no successes" is not a small sample: on this fleet it was
  reached within the first hour of the outage. On the 02:06 numbers this
  fleet classifies `dead` on every voter.

`dead` and `degraded` emit one `tracing::warn!` per hour per node from the
same loop (`"fragment-index queue is {verdict}: {claims} claims, {ready} ready in 24h"`)
and a new gauge family `plurx_analysis_queue_health{verdict}` = 1 for the
current verdict, rendered by a new block in `AnalysisRuntimeMetrics::prometheus`
(`state.rs:1225-1298`, which today renders fixed histograms and counters).
`GET /api/v1/analysis/summary` and the `analysis` block of
`GET /api/v1/activity/detail` read the published sample. The Analysis view
renders the verdict as its first line in the tone colours the row table
already uses.

### 3.6 Bulk reopen (M4)

```
POST /api/v1/analysis/reopen
{ "component": "fragment_index",
  "error_codes": ["attempt_limit", "queue_expired"],   // default
  "failed_after_ms": 1787000000000,                     // optional window
  "failed_before_ms": 1788480000000,
  "limit": 500,
  "dry_run": true }
→ 200 { "matched": 1982, "reopened": 0, "skipped_active": 0, "dry_run": true }
```

Admin-only, behind the same `analysis_queue_enabled` gate as
`POST /files/{id}/analysis`. It is a server-side loop over
`retry_analysis_request_admin` — the per-row retry button's primitive — for
every **request** row (`component`, `state = 'failed'`, whose own
`last_error_code` or whose joined job's code is in the filter). Each match
gets a forced successor request with a fresh generation; the failed row is
never mutated. The 26 request-level deaths with no job row are matched by
the request's own code.

Why the successor and not an in-place reset (the first draft of this
document proposed flipping rows back to `queued`/`submitted`, and it was
wrong three ways):

- CONTENT-ANALYSIS-INDEX-HANDOFF §5.3 ("force allocates a new requested
  generation"), §5.4 (exact fences), and §6.8 ("do not write a stuck-job
  reaper that mutates jobs — two owners is the bug") forbid it, and
  `analysis_requests_one_active_source` would fail a whole batch `txn` the
  moment two rows share an identity.
- Reopening 1,933 job rows at once turns most of them into `queue_expired`:
  the sweep expires any `queued` row older than 6 h, and at 12–16 builds an
  hour the queue consumes ~90 rows in that window. A successor *request*
  only becomes a job when resolution admits it (2 per tick per node), which
  throttles job creation to roughly the build rate, so the 6 h window is
  never hit.
- `analysis_requests_one_active_forced_successor` admits one active forced
  request per file. A file with four dead targets reopens on one target and
  the other three report as `skipped_active` — which is the M5.1 dedup for
  free, and why `limit` should be counted in files, not rows.

`MAX_ANALYSIS_REQUESTS = 4096` active requests (`:28`, checked at
`:1053-1054`) and `MAX_ACTIVE_JOBS = 4096` are the caps; 617 keys plus the
639-request backlog fits, and `limit` exists so an operator can stay under
them deliberately.

Why an endpoint and not a SQL fix: the jobs table is Raft-replicated state.
Editing one node's SQLite file diverges that node from the log and is
exactly the kind of repair the cluster is built to refuse. Why not automatic
on upgrade: an amnesty that reopens every `attempt_limit` row would also
reopen rows that died for real reasons, and after M2 the operator can see
the difference before choosing.

---

## 4. Non-goals — guardrails for whoever builds this

- **Do not change `validate_parameter_order`** to accept out-of-order
  placeholders. It is correct: hiqlite binds by position and SQLite indexes
  `$N` by first appearance, so relaxing it would replace a loud refusal with
  silently swapped bindings. The statements are wrong, not the validator.
- **Do not add a census exemption marker** to production statements (§3.2).
- **Do not raise `analysis.max_attempts`, the lease, or the backoff** to
  "give jobs more time". No amount of time helps a renew that is refused
  before it is sent, and larger values only slow the discovery of the next
  such defect.
- **Do not make the heartbeat ignore `Err`** past the lease deadline (M1
  bounds the tolerance). A worker that keeps building past a lease another
  node has reclaimed produces the double-build M5 exists to prevent.
- **Do not edit the state machine SQLite files on the nodes** — not the jobs
  table, not the `analysis_requests` tombstones — see §3.6.
- **Do not mutate a terminal request or job in place** to reopen it; use the
  successor-generation primitive (§3.6).
- **Do not change discovery rate, targeting, or the one-identity-per-request
  rule in this series** beyond what M5 specifies and §8 rules. The queue was
  producing 12–16 artifacts an hour under the current rules before
  `ce253a55`, and the first job is to get back to that.
- **Do not bump `PROTOCOL_VERSION` or `SEGPLAN_VERSION`.** Nothing here
  changes a wire shape or an index identity. Bumping `SEGPLAN_VERSION`
  would re-key every `pipeline_sha256` and turn the whole library into new
  work — which is a way to "fix" coverage that hides the real repair. A
  schema bump (M2) is not a protocol bump, but it is a stop-the-fleet event
  (§3.4).

---

## 5. Milestones

**M0 is one ordinary PR to `main`** with the affected-surface gate — it
ships alone and should not wait. **M1–M5 are task PRs on
`effort/fragment-index-queue-repair`** per DEVELOPMENT_PIPELINE.md, promoted
together. One adversarial review per PR (Paul's standing rule), two
independent reviewers when the PR touches the store.

### M0 — turn the queue back on

**M0.1** Reorder the two statements per §3.1, both backends. Reorder the four
`dv_conversions` statements (§1.5) the same way, keeping the lease params
contiguous.

**M0.2** Store-contract coverage that pins the statement the daemon actually
sends (a test on an extracted helper leaves it unpinned): in
`crates/plurx-core/tests/store_contract.rs`, a scenario that enqueues, claims,
**renews** (asserting `Ok(true)` and that `lease_expires_ms` advanced),
**yields** (asserting `Ok(true)`, `state = 'queued'`, `attempts` back to 0,
`last_error_code = 'node_local_refusal'`), re-claims, and completes — run
through `dyn Store` via `for_each_backend` so it executes on SQLite and,
under `hiqlite-contract-tests`, on a real three-voter hiqlite. Mutation
check: revert M0.1's hiqlite reorder alone and confirm the hiqlite run fails
on the renew assertion while the SQLite run stays green — that asymmetry is
the point, and it documents why SQLite-only coverage was not enough.
`for_each_backend` starts a fresh cluster per test, so filter to the new
scenario: minutes, not seconds.

**M0.3** The census test per §3.2, over every `hiqlite_*.rs` store file,
replacing (or generalising) the sessions-module one. Mutation check: swap
the first appearances of two placeholders in any production statement and
the test names the file, the statement, and the ordinal it expected.

**M0.4** The ledger row the history audit demands for a runtime fix
(`fix(store): …` subject): `validation/regressions.d/<sha8>-fragment-index-lease-renew.toml`
with `points = ["core.media", "cluster.auth"]` (the store files map to
`core.media`; `hiqlite_publication.rs`, `state.rs` and `store_contract.rs`
to `cluster.auth` — re-verify in `validation/points.toml`), `checks =
["rust-gate", "rust-gate-ci", "cluster-auth"]`, and a `reason` naming the
M0.2 scenario and the census by test name, in the shape of
`4da3bbde-hiqlite-media-session-placeholders.toml`. If the row is renamed
after a rebase, verify with `git show :path` that the index — not just the
worktree — carries it.

**Acceptance:** `make cluster-store-check` filtered to the new scenario green
on both backends; census green; `cargo test -p plurxd` green. After deploy:
`cluster_fragment_index_jobs` gains `ready` rows within one `vod_index_mins`
tick on any voter, and `plurx_analysis_lifecycle_total{event="lease_loss"}`
stops increasing (it will not decrease; it is cumulative).

### M1 — a lost lease is a logged, counted event

**M1.1** `run_lease_heartbeat` per §3.3, used by the job and request
heartbeats; `Ok(false)` stops, `Err` retries until the deadline arm, every
transition logged; heartbeat stopped **before** the outcome write.

**M1.2** `record_job_outcome` wrapping the eighteen call sites;
`Ok(false)`/`Err` logged at `warn` with the identity fields. The `let _ =`
pattern disappears from `run_cluster_fragment_index_job` except for
`record_fragment_index_source`.

**M1.3** A new per-process family `plurx_analysis_lease_total{event}` with
`event ∈ {renewed, renew_failed, lost, expired, outcome_write_lost}` — not
`plurx_analysis_correlation_total`, whose HELP text is "Series
marker-correlation outcomes" (`state.rs:1283`).

**Acceptance:** unit tests on `run_lease_heartbeat` with closures: a renew
that returns `Err` twice then `Ok(true)` does not cancel `lost`; `Ok(false)`
cancels it; `Err` forever cancels it at the deadline and not before. There
is no automated proof of the heartbeat under real Raft in the tree —
`plurx-cluster-check` has no fragment-index scenario and
`cluster_activity.rs` drives the node-local indexer with the cluster cache
off — so either add a daemon scenario with `playback.vod_index_cluster_cache`
on that claims one job and asserts a `ready` row, or say in the PR that the
store-contract lane (M0.2) is the automated proof and the fleet (§6.1) is
the rest.

### M2 — the row remembers why it died

**M2.1** `attempt_errors` column and the full schema step per §3.4, both
backends, parity test extended (first to v22/v41, then to this step).

**M2.2** Every writer in the §3.4 table appends; every reopen resets; yields
and cancellations do not append.

**M2.3** History CTE, `JOB_COLS`/`JobRow`, both `/analysis/jobs` endpoints,
and the Analysis view row carry it. Replace the `attempt_limit` operator text
with one that points at the history now shown beside it.

**Acceptance:** store-contract scenario drives a job to `attempt_limit`
through five **charged** codes — `source_unavailable`, `lease_expired` via
the claim sweep, `source_attestation_failed`, `source_catalog_read_failed`,
`local_publish_failed` — with one uncharged yield in the middle, and asserts
`attempt_errors` lists exactly the five in order while `last_error_code =
'attempt_limit'` and the yield left no entry. Web test: the row renders the
list. Mutation: delete the sweep's append and the contract fails on the
`lease_expired` position. A `replicated_v26_…` migration test opens a v25
store and reads `''` from a pre-existing row.

### M3 — the queue can say it is dead

**M3.1** `health` block per §3.5, computed in `store_metrics_loop`; the
hourly `warn!`; the `plurx_analysis_queue_health` gauge; summary and
activity-detail read the sample.

**M3.2** Analysis view: verdict line first, with the numbers and the
last-ready age ("last index 2 d 7 h ago"). `dead`/`degraded` use the
`bad`/`warn` tones already defined at `index.html:11090`.

**M3.3** OPERATIONS.md gains a "How to read it" paragraph for the verdict.

**Acceptance:** a unit test on the verdict function: 25 claims / 0 ready →
`dead`; 25 / 20 / 2 losses → `healthy`; 0 / 0 → `idle`; 25 / 3 / 12 losses →
`degraded`. An HTTP test asserts the summary carries the sampled block and
that `/metrics` still passes `prometheus_scrape_has_no_store_operation`. Web
test asserts each verdict maps to a distinct sentence (same shape as
`clusterObservationReason`'s table test, so a verdict cannot ship without
wording). A replay of the 02:06 UTC numbers in §1.3 must classify `dead`.

### M4 — reopen the 617 keys

**M4.1** `POST /api/v1/analysis/reopen` per §3.6: a loop over
`retry_analysis_request_admin`, dry-run first, `limit` in files, `skipped_active`
reported.

**M4.2** Analysis view: a "Reopen failed jobs…" action that calls it with
`dry_run` first and shows the count before asking for confirmation.

**Acceptance:** store-contract scenario: three `attempt_limit` requests for
three files, one `unsupported`, one file with two dead targets; reopen with
the default filter → exactly four forced successor requests (one per file),
one `skipped_active`, the `unsupported` row untouched, no failed row
mutated. Fleet (Paul's hands, after M0 is deployed and §6.1 has confirmed
builds resume): dry-run reports ≈617 files, the real run reopens them, and
`ready_24h` climbs at the pre-outage rate over the following days.

### M5 — one build per identity, and every identity gets built

This milestone is the "coverage" half of the findings document (§1.5 there,
Q4 here) and the "P7 has never been reached" half of the DV-P7 document. It
needs the rulings in §8 before it is built; the analysis is recorded now so
the ruling is informed.

**M5.1 — duplicate builds.** Discovery on each voter targets itself
(`state.rs:3147-3151`), requests dedup on `target_node_id`, and
`submit_fragment_index_analysis` inserts a queued job for that target even
when `cluster_fragment_index_artifacts` already holds the `cache_key` built
by another voter. 363 of the 617 dead keys have a row for all four nodes; on
a healthy queue each of those is four full-bitstream passes over the same
file for one artifact. Proposal: in `run_cluster_fragment_index_job`,
between the pipeline match and the build, if an artifact for `job.cache_key`
exists and `hydrate` (`crates/plurxd/src/fragment_index_cluster.rs:486`) can
fetch it from a holder, install it locally and settle the job through a
**new** store method `complete_cluster_fragment_index_by_hydration` —
`complete` itself refuses an artifact whose `built_by_node_id` is not the
job's owner (`:2942`), and relabelling the struct to get past that would
only work because the artifact INSERT is `DO NOTHING` (`:2983`), which is a
trick, not a contract. The comment at `state.rs:6233-6236` chose "submit an
ordinary worker instead of hydrating" because hydration could not settle
the *request* fence; settling from inside the claimed *job* keeps every
fence intact. Expected effect: build work drops by up to the voter count for
files already indexed anywhere.

**M5.2 — the second and third identity.** A request resolves one identity
(`fragment_index_requested_video_options`, `state.rs:1784`: the first
identity lacking a v1 index), and background discovery never issues a second
request for the same file because the first request row is the dedup
tombstone. So a Dolby Vision file gets its stripped identity and never its
preserved or converting one from the background — only a play attempt
(foreground) or an admin request can ask. Proposal: the background
`requested_generation` for `fragment_index` includes the identity's argv
fingerprint, so one file yields one request per identity it lacks;
discovery's `attempted` continues to count files. Three consequences the
builder must handle, not discover:

- Changing the generation changes the tombstone key library-wide, so the
  next discovery pass re-requests every file. For a file whose
  `(cache_key, target)` job is `failed / attempt_limit`, `submit` is refused
  (`:1536-1549`) and the resolver maps that to `Retry { queue_full_or_busy,
  charge_attempt: false }` (`state.rs:6259-6270`) — an **uncharged infinite**
  retry loop, two claims per tick per node. M4 must have run before M5.2
  deploys, and that mapping needs its own fix regardless: a submit refused
  because the job is terminal is a terminal request (`job_terminal`), not a
  busy queue.
- `resolve_analysis_request` must derive the identity from the request
  (store the fingerprint on the request, or re-derive from the generation),
  not from "first identity lacking an index" — otherwise three requests with
  distinct generations resolve to the same job and two settle `ready`
  falsely.
- The forced-successor cancellation in `enqueue_analysis_request`
  (`:1086-1160`) cancels every active normal request for the file and
  pipeline version regardless of generation, so a force on one identity
  would kill the other two. Scope it to the generation.

**M5.3 — rate.** 4 files per pass per voter per 15 min is 384 files/day per
voter; with M5.1 the voters stop duplicating and the effective library rate
becomes ~1,150/day, or ~5 days for 5,847 files — before counting the
attestation bottleneck: 2 requests per tick per node, each a full-file
SHA-256 over NAS under a 10-minute timeout on a memo miss (160 timed out),
which is why 639 background requests sit unresolved while discovery adds
~48 an hour fleet-wide. The findings document's "is 4 the right number" is
really "is the attestation the right cost": a memo hit is free, a miss is
the whole file. Recommend leaving `INDEX_MAX_PER_PASS` alone and measuring
after M0 + M5.1, then deciding whether to raise `MAX_REQUESTS_PER_PASS` or
hash lazily.

**M5.4 — priority for a converting identity.** A P7 title without its
converting index is unplayable *as graded*; a non-DV title without an index
loses only the VOD path. `ANALYSIS_FORCED_PRIORITY_BOOST_MS` (5 min) already
orders `forced`/`foreground` ahead, but `forced` also means `force_rebuild`,
so it cannot be borrowed. The jobs table has no identity-class column
(`pipeline_sha256` is opaque), so an ordering key is a schema change — fold
it into the M2 step if §8 rules yes, or leave it until M5.2 has made the
identities requestable and the need is measured.

**Acceptance (M5.1):** store + hydrate test: two voters, one artifact built
by A, B's targeted job completes without invoking the builder (count builder
calls) and B gains a location row. **M5.2:** discovery on a DV P7 file with
`convert` enabled yields three requests with distinct generations, each
resolving to its own job; a non-DV file yields one; a request whose job is
terminal settles `job_terminal` on the first resolution, uncharged loop
gone.

---

## 6. Verifying on the fleet

Paul deploys; this is what to watch.

### 6.1 Within one tick of M0 landing

```bash
# On any node: ready jobs must start appearing again
ssh pjunod@192.168.4.14 'sudo sqlite3 /srv/plurx/hiqlite/state_machine/db/plurx.db \
  "select datetime(max(updated_at_ms)/1000,\"unixepoch\") from cluster_fragment_index_jobs where state=\"ready\""'
# Lease losses must stop climbing (compare two reads a few minutes apart)
curl -s http://192.168.4.14:32400/metrics | grep 'lifecycle_total{event="lease_loss"'
# No running row may sit past its lease for more than one tick
ssh pjunod@192.168.4.14 'sudo sqlite3 /srv/plurx/hiqlite/state_machine/db/plurx.db \
  "select count(*) from cluster_fragment_index_jobs where state=\"running\" and lease_expires_ms < (strftime(\"%s\",\"now\")*1000)"'
```

Reading the live WAL-mode file with `sqlite3` takes a shared lock for
milliseconds; copying the `db`, `-wal`, and `-shm` to `/tmp` first, as the
evidence run did, is the polite version.

**How to read it:** `max(updated_at_ms)` of `ready` advancing and
`lease_loss` flat is the fix working. Ready advancing but `lease_loss` still
climbing means a *different* lease problem is also present (the M1 log lines
will name it). Neither advancing means the drain is not running — check
`pretranscode_worker_idle` (any active transcode blocks it) and that
`playback.vod_index_cluster_cache` is still on.

### 6.2 After M4's reopen

`ready` rows per day should return to the 08-30 shape (~230 distinct files a
day) until the 617 keys are consumed. The `attempt_errors` histogram over
any new failures is the first thing to read if it does not.

### 6.3 The DV-P7 experiment from the companion document

`POST /api/v1/files/70/analysis` was issued (force=true, 02:00:14 UTC, request
`16c35d03…`) by the other session. Before M0 that request can only die at
`attempt_limit` like the rest; after M0 it is the cheapest end-to-end check
that a converting identity builds — it is still the *requested* identity
choice at `state.rs:1784` (the first identity lacking a v1 index), so if file
70 already holds a stripped v1 index the converting one is what gets built.

---

## 7. The review questions, answered

From `FRAGMENT-INDEX-HEALTH-FINDINGS.md` §5:

1. **Is 30 % normal, or is 09-01 an event?** Neither. The success rate since
   the 2026-08-31 19:00 UTC deploy is 0 %; the 30 % is the 932 artifacts built
   *before* it divided by everything. 09-01 is the backlog draining into the
   defect at full speed, not a separate event.
2. **Preserve the error across retries?** Yes — M2, one column, appended by
   every charged writer including the sweep. The aggregate lifecycle
   counters already held the answer this time; they will not the next time
   two causes overlap.
3. **Surface queue health?** Yes — M3's verdict. Note the correction in
   §1.4: counters and a job list existed; a sentence did not.
4. **Is 4 per pass right?** It was producing 12–16 artifacts an hour before
   the outage and the ceiling is the attestation, not the pass size. M5.3:
   measure after M0 + M5.1, then decide.
5. **Are the `running` and `attempts = 0` rows stuck?** The `running` rows
   are the defect itself (§1.3); they clear at the next claim sweep and
   stop appearing after M0. The `attempts = 0` *job* rows are waiting for a
   slot behind hundreds of retries; the 639 `attempts = 0` *request* rows
   are the attestation backlog (M5.3). Both clear after M0, the second more
   slowly.

From `DV-P7-CONVERSION-NOT-REACHING-PLAYBACK.md` §6:

1. **Coverage or a second logic defect?** Neither exactly: it is *this*
   defect. No P7 file has a failure row because the cursor had not reached
   them before the queue died, and since then nothing has been built for any
   file. §3.2 there is consistent; the cause is upstream of it.
2. **Prioritise converting identities?** Only meaningful after M5.2 makes
   them requestable at all; M5.4 sketches the ordering and its cost.
3. **Count the silent degradation?** Agreed, and out of scope here; it
   belongs with M3 as a playback-side counter
   (`plurx_playback_dv_conversion_degraded_total`), one line in the same PR.
4. **Should `/decision` stop promising a conversion whose index is absent?**
   Paul's ruling (§8.3). The clients handle a badge flip today; whether an
   honest HDR10 up front is better is a product call, not a defect.

---

## 8. Rulings needed from Paul

1. **M5.1 — settle a targeted job by hydration when the artifact exists.**
   Saves up to 4× the build work; adds one store method; `built_by_node_id`
   stays the original builder and the location row says who holds it.
   Recommend yes.
2. **M5.2 — one request per missing identity vs one request fanning out to
   N jobs.** Recommend per-identity requests (keeps the 1:1 request → job
   relation `settle_analysis_requests` and the history CTE assume), with the
   three consequences in M5.2 built in the same PR and M4 run first.
3. **DV-P7 Q4 — promise the conversion or badge HDR10 until the index
   exists.** No recommendation; both are honest.
4. **M4 — reopen by operator action, once, with dry-run.** Recommend yes;
   the endpoint stays for the next time. The alternative — an automatic
   amnesty on upgrade — cannot tell a lease-loss death from a real one until
   M2 has been running.
5. **M5.4 — an identity-class ordering column in the M2 schema step, or
   later.** Recommend later, measured.

---

## 9. Evidence — how the numbers in §1 were read

Replicated state machine copied on m6 (`/srv/plurx/hiqlite/state_machine/db/
plurx.db` + `-wal` + `-shm` → `/tmp/fiq/`), queried with the host's
`sqlite3`. Any node gives the same answers; nuc3 is a learner and answers
`learner_route_ineligible` on the API but replicates the same log.

```sql
-- outcome × code, with attempt bounds
SELECT state, last_error_code, COUNT(*), MIN(attempts), MAX(attempts)
  FROM cluster_fragment_index_jobs GROUP BY 1,2 ORDER BY 3 DESC;
-- failures per day
SELECT date(updated_at_ms/1000,'unixepoch'), last_error_code, COUNT(*)
  FROM cluster_fragment_index_jobs WHERE state='failed' GROUP BY 1,2;
-- the per-code retry record that names the cause
SELECT event, reason, count FROM analysis_lifecycle_counters;
-- when building stopped
SELECT strftime('%Y-%m-%d %H', built_at_ms/1000,'unixepoch'), COUNT(*)
  FROM cluster_fragment_index_artifacts GROUP BY 1;
-- row lifetime histogram of the dead jobs (minutes)
SELECT (updated_at_ms-created_at_ms)/60000, COUNT(*)
  FROM cluster_fragment_index_jobs
 WHERE last_error_code='attempt_limit' GROUP BY 1;
-- stale running rows and how close together they were claimed
SELECT owner_node_id, attempts, fence, updated_at_ms, lease_expires_ms
  FROM cluster_fragment_index_jobs WHERE state='running';
-- duplication across targets
SELECT n, COUNT(*) FROM (SELECT file_id, COUNT(DISTINCT target_node_id) n
  FROM cluster_fragment_index_jobs WHERE last_error_code='attempt_limit'
  GROUP BY file_id) GROUP BY 1;
-- the request backlog, by target
SELECT target_node_id, COUNT(*) FROM analysis_requests
 WHERE state='queued' AND attempts=0 GROUP BY 1;
-- request-level deaths with no job row
SELECT COUNT(*) FROM analysis_requests
 WHERE state='failed' AND last_error_code='attempt_limit'
   AND COALESCE(result_cache_key,'')='';
```

A reviewer's hypothesis that the 639 queued requests were a settle/resubmit
loop over `ready` jobs lacking a location row was checked and refuted: the
join returns 0. They are simply unresolved.

Fleet state at the time: all four nodes on `v0.3.0-464-g9646f99f` /
`-466-gc967d6db`, containers recreated 01:25–01:32 UTC (so container logs
predating the deploy are gone — the 09-01 daemon logs the findings document
wanted no longer exist; the counters and the row timestamps are what
remain). Store write metrics on m6's current process: 4,177 ok, 0 error. The
`WARN hiqlite::split_brain_check` line every 7.5 s with an empty message is
unrelated background noise worth its own look some other day.
