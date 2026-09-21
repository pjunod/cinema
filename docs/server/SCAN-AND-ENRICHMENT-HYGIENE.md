# Scan and enrichment hygiene — walk off the runtime, deadlines on every provider call

**Status:** adversarial findings addressed; draft waiting on P-01 #401's shared fast-lane repair · **Executes:** C3 / F-core-3 and C5 / F-core-6
from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
(assessment rows C3, F-core-3, C5, F-core-6 in
[ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md))
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Read §2 first: it quotes the walk, the per-file stat, the two provider
clients and the join request as they are, and states what the job lease
does and does not do when one of them hangs. Then build §5 in order — M1
(the scanner), M2 (provider deadlines), then M3 (`post_join_request`). The
current workboard protocol keeps all three logical milestone commits in one
draft implementation PR into `main`. Every `file:line` is from `88a3957a`;
re-verify by function name.

**If a step seems to require changing candidate order, the reconcile
guard (`walk_errors == 0`), the size+mtime unchanged short-circuit, the
prune limit, or the retry semantics of a 404 from TMDB, stop and flag
it.** Those are the invariants the scanner's correctness rests on; this
plan moves work between threads and adds deadlines, nothing else.

**Correction to the review:** none of substance. One precision: the
review's C5 says a provider hang "blocks every node" because enrichment
runs under a leased singleton. Read from `job_lease.rs:37-38, 85-115`, the
lease (`JOB_LEASE_TTL = 90 s`, heartbeat every 30 s) is renewed by a
*separate* task, so a hung provider call does not lose the lease — it keeps
it. The consequence is therefore stronger than "blocks until the lease
expires": the `scan:library:<id>` job stays held indefinitely on the hung
node and no other node can scan that library until the process restarts.
The assessment asked for this to be verified rather than derived; it is
now read from the code, and M2's acceptance covers it.

---

## 1. Objective

1. A library scan never runs a directory walk or a per-file `stat` on a
   tokio worker thread. A slow NAS slows the scan; it does not slow every
   HTTP request the same worker would otherwise poll.
2. The walk is bounded in memory-in-flight and observes cancellation: when
   the scan's lease is lost, the walker stops within one page rather than
   walking to the end of a 200k-file export for nobody.
3. Order, progress, error accounting and the reconcile guard are
   byte-for-byte what they are today. The existing scan tests pass without
   edits to their assertions.
4. Every TMDB and AniList request has a connect deadline, a total deadline
   that includes the body, a bounded body, and a bounded retry budget; every
   *item* has a deadline, so one title that TMDB will not answer for cannot
   hold a library's enrichment — and the lease with it — forever.
5. `post_join_request` is bounded the same way, and a timeout is reported
   as *ambiguous*, not as "did not happen".

## 2. Contract today

Re-verify at build time.

### 2.1 The walk and the stat are synchronous on the runtime

`crates/plurx-core/src/scan/mod.rs:494-513` (inside the async
`scan_library_with_publication_and_prune_limit`, called from
`state.rs:5272` under the scan lease):

```rust
for entry in WalkDir::new(root).follow_links(true) {
    match entry {
        Ok(entry) => {
            if entry.file_type().is_file() && wanted_file(library, entry.path()) {
                candidates.push(entry.into_path());
            }
        }
        Err(e) => {
            report.errors += 1;
            walk_errors += 1;
            // … tracing::error! + report.note("cannot read `{at}`: {e}")
        }
    }
}
// …
candidates.sort();
if let Some(p) = progress { p.found.store(candidates.len(), Ordering::Relaxed); }
```

The same loop exists for the targeted scan at `:832-850` (it walks one
canonical path; its walk errors are reported but never fatal because there
is no reconcile). `library_root_fingerprint` (`:704-724`) calls
`canonicalize()` on every root synchronously before the walk.

`file_stat` (`:358-368`) is `std::fs::metadata`, called per candidate from
`record_candidates` (`:957`) and per file from the re-probe pass (`:234`).
A stat failure is deliberately *not* "gone" — the path stays in `seen` so
reconcile keeps its row (`:960-973`).

There is no `spawn_blocking` anywhere in `crates/plurx-core/src/scan/`
(grep at `88a3957a`). Other synchronous reads in the module — `exif.rs:18`
(`File::open` for EXIF), `nfo.rs:190` (`fs::read` of an NFO),
`recordings.rs:87` (`fs::read` of a sidecar), `probe.rs:107` (a stat of
the ffprobe binary) — are per-file, small, and out of this plan's scope;
they are listed so nobody claims the module is clean after M1.

The invariants that must survive:

- **Order.** Candidates are processed in `sort()`ed order, so placement,
  duplicate-directory reporting and note order are deterministic.
- **Progress.** `ScanProgress.found` is stored once, after the walk;
  `processed` and `changed` advance per file (`:286-293, 951-953`).
- **Reconcile guard.** Vanished-row deletion runs only when
  `walk_errors == 0` (`:554`), bounded by `prune_limit`. A partial walk
  must never delete.
- **Cancellation.** The whole scan future is dropped when the lease-loss
  token fires (`state.rs:4370-4375, 5165-5170`, `tokio::select!` against
  `lost.cancelled()`). Nothing inside the scan polls the token.

### 2.2 The provider clients have no deadlines

`crates/plurx-core/src/metadata/tmdb.rs:137-145`:

```rust
http: reqwest::Client::builder()
    .user_agent(concat!("plurx/", env!("CARGO_PKG_VERSION")))
    .build()
    .unwrap_or_default(),
```

`anilist.rs:57-62` is identical. reqwest's builder has no connect or
total timeout by default. Around it: `send_with_retry` (`tmdb.rs:192-231`)
retries `MAX_ATTEMPTS = 3` with `RETRY_BASE_DELAY = 500 ms` doubling, obeys
`Retry-After` capped at `RETRY_AFTER_CAP = 30 s` (`:25, 502-511`), and
retries only 429/5xx and connection errors (`retryable`, `:495-497`) —
404 stays a fast permanent failure, and that rule is load-bearing.
`download_image` (`:470-485`) reuses the retry and bounds the body through
`bounded_artwork_response` (`metadata/mod.rs:79-107`, `MAX_ARTWORK_BYTES =
15 MiB`); the JSON calls (`get` → `.json()`) have no body bound. AniList's
`find_anime` (`anilist.rs:74-95`) posts a GraphQL query with no retry and no
bound at all.

Worst case per JSON call today: unbounded connect + unbounded response +
unbounded body, three times, plus up to 60 s of retry sleeps. Worst case
per item: several such calls (search, detail, keywords, two images).

### 2.3 Where the calls run, and what the lease does

`state.rs:4557-4640` `enrich()` picks the provider per library kind and
runs the whole library sequentially inside the scan job. The job is held
by an `ActiveJobLease` for `scan:library:<id>` (`state.rs:4251-4252,
5272`), whose heartbeat task renews every 30 s against a 90 s TTL
(`job_lease.rs:37-38`). Renewal failure or a renewal that overruns the TTL
self-fences (`:109, 132`); a hung *body* of work does neither. So a TMDB
request that never returns holds the library's scan job on that node with
a healthy lease until restart; other nodes see the job as owned and
requeue (`state.rs:5130-5147`).

### 2.4 `post_join_request`

`crates/plurx-core/src/cluster/migration.rs:868-897`:

```rust
let response = reqwest::Client::new()
    .post(format!("{}{path}", payload.bootstrap_http()))
    .json(request)
    .send()
    .await
    .map_err(|error| StoreError::Database(format!("contacting join coordinator: {error}")))?;
```

Called by `redeem_remote_join` (`:844-853`, from `:529`) and
`finalize_remote_join` (`:856-865`, from `:790`). No connect, total or body
deadline; a fresh client per call. Both requests carry a single-use
`token_digest`; the local staged membership (`:519-528`) records that
digest, and re-running the same token is the documented recovery for an
interrupted join (`:497-500, 826-833`).

## 3. Change

### 3.1 M1 — a bounded blocking producer for the walk; `tokio::fs` for stats

**Shape.** One `spawn_blocking` walker per scan (not per root — the roots
are walked in configured order, as today) producing pages through a
bounded channel:

```rust
enum WalkEvent {
    Candidate(PathBuf),
    Error { at: String, error: String },   // `walkdir::Error` is not Send-cheap; carry Display
    RootNotDirectory(PathBuf),
}
const WALK_PAGE: usize = 256;
const WALK_PAGES_IN_FLIGHT: usize = 4;      // 1,024 paths buffered at most

fn spawn_walker(roots: Vec<PathBuf>, library: WalkFilter)
    -> (tokio::sync::mpsc::Receiver<Vec<WalkEvent>>, tokio::task::JoinHandle<()>)
{
    let (tx, rx) = tokio::sync::mpsc::channel(WALK_PAGES_IN_FLIGHT);
    let handle = tokio::task::spawn_blocking(move || {
        let mut page = Vec::with_capacity(WALK_PAGE);
        for root in roots {
            if !root.is_dir() { page.push(WalkEvent::RootNotDirectory(root)); continue; }
            for entry in WalkDir::new(&root).follow_links(true) {
                page.push(match entry { /* exactly the two arms at :495-511 */ });
                if page.len() == WALK_PAGE
                    && tx.blocking_send(std::mem::take(&mut page)).is_err() { return; } // consumer gone
            }
        }
        let _ = tx.blocking_send(page);
    });
    (rx, handle)
}
```

`WalkFilter` is the `(LibraryKind, anime, …)` subset `wanted_file` reads,
copied out of `&Library` so the closure is `'static`.

**The consumer keeps today's semantics.** The async side drains `rx`,
appending `Candidate` paths to the same `candidates: Vec<PathBuf>` and
applying `Error`/`RootNotDirectory` exactly as `:483-511` do (`report.errors
+= 1`, `walk_errors += 1`, the same `tracing::error!` and `report.note`
text). After the channel closes it runs `candidates.sort()` and the rest of
the function unchanged. This is deliberate: processing pages as they arrive
would change candidate order from sorted to walk order, and §2.1 lists
order as an invariant. The memory ceiling for the candidate list is
therefore the same as today (one `PathBuf` per media file); what the bound
buys is (a) no runtime worker pinned for the walk, and (b) a cancellation
point every 256 entries — when the scan future is dropped, `rx` drops, the
next `blocking_send` fails and the walker returns.

`ScanProgress.found` advances per page (`fetch_add(page_candidates)`)
instead of once at the end; the final value is identical and the field's
documented meaning ("candidate media files discovered by the directory
walk") still holds. The two scan tests that read `found` assert the final
value; they keep passing.

**What a blocked syscall does.** `spawn_blocking` work cannot be
interrupted (assessment F-core-3). A walker stuck in `readdir` on a dead
NFS export stays stuck until the kernel returns; the scan future has
already been dropped and the lease released. The plan accepts this and
bounds its blast radius: at most one walker per library scan, scans are
lease-serialised per library, and the blocking pool is tokio's (512
threads default), so one orphaned thread per stuck export is the cost.
M1 logs `walker orphaned: consumer gone before walk finished` at `warn`
when `blocking_send` fails, with the root path, so a stuck export is
visible in `Settings → System → Logs`.

**The targeted walk** (`:832-850`) uses the same `spawn_walker` with one
root and its own error arm (never fatal, no reconcile). Same page size.

**`file_stat`** becomes `async fn file_stat(path) -> io::Result<(i64, i64)>`
over `tokio::fs::metadata` (one `spawn_blocking` hop per file, sequential,
as the review specifies). Both callers (`:957`, `:234`) `.await` it; the
error arms are unchanged. `library_root_fingerprint` (`:704`) is wrapped
in one `spawn_blocking` at its two call sites (`:463`, and the targeted
path's canonicalize at `:802`), not changed itself — it is also used
synchronously by tests and the CLI.

**Not done in M1.** Stat-at-walk (letting the walker return
`entry.metadata()`) would halve the syscalls but moves the stat from
process time to walk time, changing what the unchanged short-circuit
compares for a file that grows during the scan (a recording in progress).
Left as §7.1 with the measurement that would justify it.

**Metric.** `plurx_scan_walk_seconds{phase="walk|process"}` histogram
(two label values; buckets 1, 5, 15, 60, 300, 900, +Inf), rendered beside
`plurx_scan_total` in `http/system.rs:5015-5030`. Walk time on a runtime
worker was invisible; this makes the NAS cost a number.

### 3.2 M2 — provider deadlines, body bounds, retry budget, per-item deadline

**Client construction** (`tmdb.rs:137`, `anilist.rs:57`):

```rust
reqwest::Client::builder()
    .user_agent(concat!("plurx/", env!("CARGO_PKG_VERSION")))
    .connect_timeout(Duration::from_secs(5))    // PROVIDER_CONNECT_TIMEOUT
    .read_timeout(Duration::from_secs(10))      // PROVIDER_READ_TIMEOUT: a stalled body
    .timeout(Duration::from_secs(30))           // PROVIDER_TOTAL_TIMEOUT: connect → body end
    .redirect(reqwest::redirect::Policy::limited(3))
    .build()
    .unwrap_or_default(),
```

reqwest's `timeout` runs from the start of the request until the body has
been fully read, so a `.json()` or `bytes_stream()` that stalls is covered;
`read_timeout` catches a body that trickles one byte every 9 s inside that
window. The `unwrap_or_default()` fallback stays but now logs at `warn`
once — a default client is the unbounded one.

**Body bound on JSON.** `TmdbClient::get` and `AniListClient::find_anime`
read the body through `bounded_json_response(resp, PROVIDER_JSON_MAX_BYTES
= 4 MiB)` — the same shape as `bounded_artwork_response`
(`metadata/mod.rs:79`) — then `serde_json::from_slice`. TMDB's largest
routine document (a show with `append_to_response`) is under 1 MiB;
4 MiB is headroom, not a guess to revisit weekly.

**Retry budget.** `send_with_retry` keeps `MAX_ATTEMPTS = 3`, the 404 rule
and `Retry-After`, and gains a wall budget: `PROVIDER_CALL_BUDGET = 60 s`
measured from the first attempt; an attempt is not started if the
remaining budget is below `PROVIDER_CONNECT_TIMEOUT`, and a `Retry-After`
longer than the remaining budget ends the call with
`MetadataError::Status(429)` rather than sleeping past it. Three 30 s
attempts could otherwise total 90 s plus 60 s of sleeps per *call*.

**Per-item deadline.** In `enrich_library_for_targets_with_publication`
(`metadata/mod.rs:574`, loop at `:629`) and the AniList twin (`:968`),
each item's provider work is wrapped in
`tokio::time::timeout(ITEM_ENRICH_DEADLINE = 120 s, …)`. On expiry the
item is recorded as a bounded problem line (`"enrichment deadline
exceeded for `Harbor Lights (2019)`; will retry on the next scan"`), the
item's attempt row is **not** marked permanent (a deadline is a transient,
like `Truncated` in the fragment indexer), and the loop continues. 120 s
is two full call budgets: enough for search + detail on a slow day, small
enough that a 2,000-title library cannot spend more than a day on
timeouts before the scheduled scan comes round again.

Why a per-item deadline and not only per-call: the item is the unit whose
partial result is safe to abandon (nothing is written until the patch is
complete), and it is the unit the operator sees in the problems list.

**A timed-out read is not proof it did not happen.** Every provider call
here is a `GET` or a read-only GraphQL `POST`; repeating one cannot double
anything, which is why the existing comment at `tmdb.rs:218-220` is
correct and stays. The plan changes no write path.

**Metric.** `plurx_provider_requests_total{provider="tmdb|anilist",
outcome="ok|status|timeout|body_bound|error"}` (two × five label values)
and `plurx_enrich_item_deadlines_total{provider}`; rendered from
`plurx-core` through the existing `prometheus_*` string-returning pattern
(`store/hiqlite.rs:678`) so `plurxd` only concatenates.

**Settings.** None. The deadlines are constants with reasons; if a
self-hosted TMDB proxy on a slow link ever needs longer, that is a
`[metadata]` config key, not a replicated setting, because the network
position is per node.

### 3.3 M3 — `post_join_request` bounded, timeout reported as ambiguous

One shared `join_client()` built with `connect_timeout(5 s)`,
`timeout(30 s)`, `redirect(Policy::none())` (a redirect could send a join
token to a host the operator never named — the same reason
`images.rs:417-419` refuses redirects). The response body is read through
the 4 MiB bound before `.json::<JoinApiError>()`.

On `reqwest::Error::is_timeout()` the function returns
`StoreError::Migration("join_request_ambiguous: the coordinator did not
answer within 30 s; the request may have been accepted — re-run the same
staged join, do not mint a new token")`. The error code is new and
distinct from `contacting join coordinator`, because the two demand
different operator actions. Recovery is the path that already exists: the
staged `LocalMembership.join_token_digest` (`:519-528`) makes re-running
the same token the documented way to resume an interrupted join. Build-time
verification (acceptance below): the coordinator's redeem/finalize handlers
must treat a repeated digest from the same `(raft_id, node_id)` as
idempotent success; if they refuse it, *that* is the fix, and inventing a
retry loop here would be wrong.

## 4. Guardrails (non-goals)

- **No unbounded queue** (C3 "an unbounded queue just moves the problem";
  F-core-3 "merely paginating results is insufficient if an unbounded
  producer queue is introduced"). `mpsc::channel(4)` of 256-entry pages;
  the walker blocks on `blocking_send`. The candidate `Vec` is the same
  size as today and is stated as such — the plan does not claim a memory
  win it does not have.
- **Order, progress, errors, cancellation preserved** (C3, F-core-3).
  Consumer collects then sorts; `found` reaches the same final value;
  the two error arms are copied, not rewritten; cancellation is by
  receiver drop and is tested.
- **Reconcile guard untouched.** `walk_errors` is incremented from the
  consumer's `Error`/`RootNotDirectory` arms exactly where `:483, 503` do
  today; a `RootNotDirectory` still counts as a walk error so a missing
  mount never deletes.
- **`spawn_blocking` is not cancellable** (F-core-3). Said in §3.1 with
  the blast radius; the orphan log line makes it observable.
- **Throughput is measured, not assumed** (C3 "the throughput loss is to
  be measured on the real core count"; assessment "network-filesystem
  delay is a measurement target"). `plurx_scan_walk_seconds` and the
  `nas` protocol in §6.3 are the measurement.
- **Deadlines include the body and the retry budget** (F-core-6 "include
  response-body consumption, retry budget and lease cancellation, not
  merely connection timeout"). `timeout` + `read_timeout` + body bound +
  call budget + item deadline, each named.
- **404 stays permanent and fast** (`tmdb.rs:490-494`). `retryable` is
  not touched; the deadline changes only how long a retryable call may
  take.
- **Ambiguity is named, not resolved by retrying** (F-core-6 "a timeout is
  not proof the operation did not happen"; "preserving reconciliation of
  an ambiguous accepted join"). M3 returns a distinct code and points at
  the existing staged-join recovery.
- **No feature gate, no new setting.** Constants with reasons.

## 5. Milestones

### 5.1 M1 — bounded walker and async stats (`core/scan-walk-off-runtime`)

1. `WalkEvent`, `spawn_walker`, the consumer loop in
   `scan_library_with_publication_and_prune_limit` and the targeted twin;
   `file_stat` async; `library_root_fingerprint` call sites wrapped.
2. Tests (all in `scan/mod.rs`'s existing test module, using its
   tempdir helpers):
   `walker_pages_are_bounded_and_ordered` (a tree of 1,100 files; assert
   pages ≤ 256, final candidate order equals today's sorted order, `found`
   equals 1,100);
   `walk_errors_still_block_reconcile` (an unreadable subdirectory via
   `chmod 000` on Unix; assert no row deleted and the note text matches
   the current one byte for byte);
   `root_that_is_not_a_directory_is_reported_by_the_consumer` (same note
   text as `:486-491`);
   `dropping_the_scan_stops_the_walker` (a walker over a slow fake tree —
   a `WalkDir` root under a directory with 10k entries — dropped after the
   first page; assert the `JoinHandle` finishes within 1 s and the orphan
   warning is logged);
   `stat_failure_keeps_the_row` (dangling symlink; unchanged assertion).
3. `cargo test -p plurx-core scan::` and the state-level scan tests
   `cargo test -p plurxd scan` green.

Acceptance: `grep -c "WalkDir::new" crates/plurx-core/src/scan/mod.rs`
still prints 2 and both are inside `spawn_walker`'s closure;
`grep -n "std::fs::metadata" crates/plurx-core/src/scan/mod.rs` finds no
line outside `#[cfg(test)]`; on `lab2` (the node with the `nas` export
mounted) a full scan of the Films library reports
`plurx_scan_walk_seconds{phase="walk"}` and, during the walk,
`curl -s -o /dev/null -w '%{time_total}\n' http://10.42.1.12:32400/api/v1/server`
stays under 50 ms (the §6.3 measurement).

### 5.2 M2 — provider deadlines and per-item budget (`core/provider-deadlines`)

1. Client builders, `bounded_json_response`, the call budget in
   `send_with_retry`, the item deadline in both enrichment loops, the two
   metrics.
2. Tests against the existing mock servers (`tmdb.rs:840-990` already
   spins an axum mock; `genre_counting_mock` at `metadata/mod.rs:1756`):
   `a_hanging_search_ends_with_a_timeout_error_inside_the_budget` (mock
   sleeps 120 s; assert error within 31 s and `outcome="timeout"`);
   `a_trickling_body_is_cut_by_the_read_timeout`;
   `an_oversized_json_body_is_refused_without_buffering_it_all`;
   `retry_after_beyond_the_budget_ends_the_call` (mock answers 429 with
   `Retry-After: 600`; assert no sleep past the budget);
   `an_item_that_exceeds_its_deadline_is_recorded_transient_and_the_loop_continues`
   (two items, first hangs; assert the second is enriched and the first's
   attempt row is retryable);
   `not_found_is_still_not_retried` (pinning `:490-494`).
3. Confirm the lease reading in §2.3 with a test in `state.rs`: a scan
   whose provider hangs keeps renewing its lease (so the statement in the
   correction is pinned, and the item deadline is what frees the job).

Acceptance: `cargo test -p plurx-core metadata::` and
`cargo test -p plurxd enrich` green; on `lab1`, with `iptables -I OUTPUT
-d api.themoviedb.org -j DROP` for one scan, the Films scan finishes with
`plurx_enrich_item_deadlines_total{provider="tmdb"}` equal to the number
of titles attempted and `Settings → System` shows the scan as done with
problems, not running; after the rule is removed the next scan enriches
them.

### 5.3 M3 — bounded join request (`core/join-request-deadline`)

1. `join_client()`, body bound, the `join_request_ambiguous` code.
2. Test: a coordinator mock that accepts then never answers; assert the
   distinct error within 31 s. Read `http/cluster.rs`'s
   `redeem_join`/`finalize_join` handlers for a repeated digest from the
   same `(raft_id, node_id)` and add
   `a_repeated_redeem_from_the_same_node_is_idempotent` if it holds; if it
   does not, stop and flag (this is a cluster-protocol change, not this
   plan).

Acceptance: `cargo test -p plurx-core cluster::migration` green;
`grep -n "reqwest::Client::new()" crates/plurx-core/src/cluster/migration.rs`
finds nothing.

## 6. Verification and rollout

### 6.1 Lanes

Per milestone the focused `cargo test` above, then `make unit` once
before un-WIP. `make validate-staged` before every push. No Node or Python
gate is affected.

### 6.2 Rollout

M1 to `lab2` first (it mounts `nas`), one full scan watched, then the
fleet. M2 and M3 to the fleet after `lab1` passes acceptance. Nothing here
changes recipe identity, cache digests or schema; rollback is the previous
`sha-` image.

### 6.3 Measurement protocol (C3's "measure on the real core count")

On `lab2`, before and after M1, three runs each:

1. Drop the page cache on the NAS-facing client
   (`sync; echo 3 > /proc/sys/vm/drop_caches`).
2. Trigger a full scan of the Films library from Settings.
3. Every second until the scan leaves the `scanning` phase, record
   `time_total` of `GET /api/v1/server` and `GET /api/v1/hubs` from a
   second host (`curl -w`), and the scan's wall time from
   `/api/v1/scan/status`.
4. Report p50/p99 of both request timings during the walk and the wall
   time, before vs after, in the PR body. The claim M1 may make is the one
   the table supports.

### 6.4 What only the fleet can prove — GPT prompt

```text
On lab1 (10.42.1.11), with the build carrying PR <M2 number>:
1. Confirm TMDB enrichment works normally: refresh artwork on one movie
   from its detail page; the poster should update within a minute.
2. Block TMDB for the process only:
   sudo iptables -I OUTPUT -m owner --uid-owner plurx -d api.themoviedb.org -j DROP
   sudo iptables -I OUTPUT -m owner --uid-owner plurx -d image.tmdb.org -j DROP
3. Trigger a full scan of the smallest movie library from Settings →
   Libraries. Record when the scan leaves "scanning" and when it leaves
   "enriching" (Settings → System). It must finish; report the elapsed time
   and the problems count.
4. curl -s http://10.42.1.11:32400/metrics | grep -E 'plurx_enrich_item_deadlines_total|plurx_provider_requests_total'
   and paste the lines.
5. Remove both rules (iptables -D …) and trigger the scan again; confirm
   the deadline counter stops rising and posters appear.
Report exact values; if the scan is still "enriching" after 30 minutes,
stop, paste the last 50 lines of journalctl -u plurxd, and do not restart.
```

## 7. Open questions

1. **Stat at walk time.** Returning `entry.metadata()` from the walker
   would remove one `spawn_blocking` hop per file. It changes the moment
   the size/mtime is read (§3.1). Decide after M1's `phase="process"`
   histogram says how much of the scan is stat time; if it is under 10 %,
   leave it.
2. **Streaming the candidate pages into processing** (drop the sort). It
   would bound candidate memory and start writing rows before the walk
   ends, but changes item-creation order and the placement of duplicates.
   Needs its own plan with the order-sensitive tests enumerated; not part
   of this one.
3. **`ITEM_ENRICH_DEADLINE` for anime.** AniList search is one call, so
   120 s is generous; if the metric shows AniList items never come near
   it, a per-provider constant is a one-line follow-up.
4. **Coordinator idempotence on a repeated join digest** (M3 step 2).
   Resolved in M3 and strengthened after adversarial review:
   `daemon_join_refuses_occupied_and_expired_targets_then_resumes_finalization`
   now drives the replicated coordinator state through reservation, an
   expired identity-bound repeated redemption, admission, and repeated
   finalization. The ambiguous error therefore directs the operator to
   re-run the same staged join on an exercised recovery path rather than a
   source-text assertion.

---

## Execution log

Executing sessions append one row per milestone (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-20 | gpt-5.6-sol | agent:/root/c02_builder | M1 | #400 | Commit `6412f1fbf7d8`: bounded four-page walker shared by full and targeted scans; async file stat and root canonicalization; walk/process histogram. `cargo test -p plurx-core scan::` (106 passed), `cargo test -p plurxd scan` (20 passed), pinned 1.97.1 checks and scoped Clippy passed. Fleet NAS timing remains post-merge evidence. |
| 2026-09-20 | gpt-5.6-sol | agent:/root/c02_builder | M2 | #400 | Commits `4995d5f6f611`, `fb98dc611a54`, and `9d766cf9169d`: bounded TMDB/AniList connect, read, total-call, retry-wall, JSON-body, and item deadlines with fixed-cardinality metrics; retryable continuation and independent lease renewal pinned. `cargo test -p plurx-core metadata::` (66 passed), `cargo test -p plurxd enrich` (6 passed), the focused lease-renewal test, pinned 1.97.1 checks and scoped Clippy passed. Fleet provider-drop acceptance remains post-merge evidence. |
| 2026-09-20 | gpt-5.6-sol | agent:/root/c02_builder | M3 | #400 | Commit `407683cc91b1`: shared bounded no-redirect join client, capped error body, distinct ambiguous-timeout recovery, and repeated same-node redemption contract. `cargo test -p plurx-core --features hiqlite-store cluster::migration` (80 passed), both focused new contract tests, pinned 1.97.1 checks and scoped Clippy passed. |
| 2026-09-20 | gpt-5.6-sol | agent:/root/c02_builder | Review #3104 | #400 | Commits `8b7c20f5c92e`, `e49591fd52eb`, and `67269a7cf766`: cancellation is observed for every walked entry even when no media page is emitted; a partially enriched multi-season show remains in the ordinary queue until every local season succeeds; replicated join state now proves expired same-identity redemption and repeated finalization after an ambiguous response. The three focused regressions and the existing ambiguous-response test passed on pinned Rust 1.97.1. PR remains draft until P-01 #401 repairs and lands the shared fast lane; fleet NAS timing and provider-drop acceptance remain post-merge evidence. |
