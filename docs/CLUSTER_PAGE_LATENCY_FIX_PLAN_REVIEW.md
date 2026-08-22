# Cluster page latency fix plan — adversarial review

**Status:** review complete · **Reviews:**
[CLUSTER_PAGE_LATENCY_FIX_PLAN.md](CLUSTER_PAGE_LATENCY_FIX_PLAN.md) and its
companion [CLUSTER_PAGE_LATENCY_REVIEW.md](CLUSTER_PAGE_LATENCY_REVIEW.md) ·
**Verified against:** `origin/main` @ `a0f9fc14` (the plan's pinned base) ·
**Re-checked against:** current `origin/main` @ `64e96aa7` (21 commits / 4
merged PRs ahead) · **Written:** 2026-08-22

This is the review the brief's §10 requested ("Requested Opus output"). Every
claim below was checked against the code at the pinned sha, not reasoned from
the plan's prose — file:line evidence is in §2 and cited inline. Where a claim
is operational (a live-fleet log count, a wall-clock timing) it cannot be
reproduced in a sandbox and is marked as operator-supplied; it is not turned
into homework.

Read §1 for the verdict, §2 for what the verification actually found, §3–§6 for
the prioritized findings in the brief's own P0/P1/P2 vocabulary, §7 for direct
answers to the plan's nine reviewer decisions, and §8 for the destructive-op
decision table.

## 1. Verdict — sound diagnosis, unusually well-verified, but written blind to a sibling plan that is already landing the overlap

**On the root cause: partially confirmed, leaning confirm.** The code-verifiable
half of the diagnosis is accurate in every particular I checked. The two
compounding faults the brief names — a broken replica (`nuc4` missing Raft log
`520000`) meeting an all-or-nothing render contract — are both real: the render
contract is exactly as described (§2), and the WAL crash window that can
manufacture a missing-log state is real and *not* closed by the existing
recovery patches (§3.3). The one link still resting on inference rather than
measurement is "Raft confirmation retries *dominate* the multi-second delay."
The brief flags this itself ("Strong inference … confirm with per-operation
timings before declaring closure"), and PR-A is designed to capture exactly that
timing. The epistemics are right; the missing evidence is scheduled, not
hand-waved.

**On the plan: approve the direction, revise before execution.** This is a
strong plan. The technical claims are precise, the SQL it proposes is
character-for-character faithful to the query it must match (§2, claim 12), the
non-goals fence off every dangerous shortcut (no time-only auth cache, no
automated voter removal, no timeout-lowering, no serving from a stale replica),
and the milestone boundaries have real rollback semantics. Two things must be
resolved before implementation begins, and both are in §3 as P0 — not because
the diagnosis is wrong, but because the plan was written against a base that is
now four merged PRs stale and is unaware of a parallel, actively-executing
in-repo plan covering the same observability ground:

1. **Reconcile with `CLUSTER-PERFORMANCE-PLAN.md` (P0-1).** That plan's P2a–P2d
   milestones — a passive local-Raft observer, a quorum commit watermark, and
   valid-gated apply-lag — merged as PRs #517/#518/#519 in the 21 commits *after*
   this plan's base. Those are the exact proof primitives PR-C and PR-D propose
   to build. This plan mentions none of them (zero occurrences of "watermark",
   "passive", "P2c", "P2d").
2. **Re-pin to `64e96aa7` and re-verify the §2/§12 boundaries (P0-2).** The plan
   says "re-verify every boundary at implementation time" — good hygiene — but
   three of the files it targets (`migration.rs`, `store/hiqlite.rs`,
   `http/mod.rs`) changed in the interim, and the sibling plan's status flipped
   from "ready to build" to "P0–P2c landed."

Everything else is should-fix or ratification. The plan does not need to be
rewritten; it needs a coordination pass and a re-pin.

## 2. What the verification found — claim-by-claim

Checked against `a0f9fc14`. Line numbers the plan cites are mostly accurate; the
Home ones drifted (the plan labels `loadHome`'s body as `viewHome`). "Confirmed"
means the code says what the plan says it says.

| # | Plan claim | Verdict | Evidence |
|---|---|---|---|
| 1 | `AuthUser` hashes the bearer token and calls `store.user_for_token` | Confirmed | `http/extract.rs:182–188` (`hash_token` :183, call :186) |
| 2 | Hiqlite token lookup is a *consistent* (leader-confirmed) read | Confirmed | `store/hiqlite.rs:1322` `query_consistent_map`; sibling non-consistent `query_map` exists at :302 |
| 3 | `TimedClient` bounds every Hiqlite op at 3 s | Confirmed | `store/hiqlite.rs:49` `STORE_TIMEOUT = Duration::from_secs(3)`; `timeout_store` wraps all methods (:1542) |
| 4 | `/readyz` = `Store::ping` (SELECT 1); `/healthz` is Store-free | Confirmed | `http/mod.rs:341` healthz constant, `:346–347` readyz→ping; hiqlite `ping` SELECT 1 at `:1037` |
| 5 | Home awaits `/libraries`+`/hubs`+`/coming-soon`, then one preview/library, ≤6 concurrent, paints only after all | Confirmed (line drift) | `web/index.html` `loadHome:3620`, Promise.all `:3623–3626`, `HOME_PREVIEW_CONCURRENCY=6:3596`; plan's "3596–3667" spans four functions |
| 6 | `/coming-soon` sits in the first Promise.all (slow → delays Home) but its failure is isolated | Confirmed | `:3625` `.catch(()=>({configured:false,entries:[]}))` |
| 7 | Home preview fan-out is `O(L)`, one request per library | Confirmed | `:3608` `/libraries/${id}/items?limit=24&sort=added` per library |
| 8 | Settings awaits exactly 7 endpoints in one Promise.all before any tab | Confirmed (exact) | `:9113–9116`; the 7 paths match; `/trakt/status` + playback-events `.catch`-isolated, other 5 can fail the page |
| 9 | Settings pulls "up to 2,000 playback events" | Confirmed | `:9116` `?since=${since7d}&limit=2000` (hard literal) |
| 10 | Activity installs `Loading…`, awaits `/activity/detail`, one non-overlapping 3 s poll, no stale fallback | Confirmed | `viewActivity:8927`, poll `3000:8931`, failure blanks whole body `:8950` |
| 11 | A generation fence exists as the single cancellation authority | Confirmed | `PAGE_RENDER_GENERATION:11843`, guards viewHome `:3664`, viewActivity `:8930/8946/8951`, viewSettings `:9117` |
| 12 | Proposed `/home/previews` window SQL matches existing `list_top_items(…,Added,0,24)` semantics | Confirmed | predicate + `ORDER BY added_at DESC, id DESC` are char-for-char identical to `store/sqlite/media.rs:364,374–375` and `store/hiqlite_media.rs:901,908–909`; genre `EXISTS` clause is a no-op at `genre=None`; window SQL parses & behaves in sqlite 3.45.1 |
| 13 | `items` columns `id/library_id/kind/parent_id/added_at` exist as named | Confirmed | `store/hiqlite_catalog.rs:34–64`, `store/sqlite/mod.rs:73–92`; sort column is `added_at` (not `added`), indexed `idx_items_added` |
| 14 | Home page-wide primitives `watch_map`/`item_max_heights`/`child_counts`/`watch_rollups` exist | Confirmed | `store/mod.rs:788/684/678/872` + both backend impls |
| 15 | `reachable` = a 30 s heartbeat window, not Raft health | Confirmed | `cluster/membership.rs:35` `NODE_REACHABLE_WINDOW_MS=30_000`, derived `:1153`; heartbeat interval 10 s `:41` |
| 16 | The offline-source probe loop sleeps 500 ms and warns on every failure, no backoff/dedup | Confirmed (line drift) | `membership.rs:47` `PROBE_POLL=500ms`; loop `1916–1935` (fn ends :1935, warn :1931) |
| 17 | `DELETE …/nodes/{id}` fences self/leader/quorum/offline-work/job-owner and tombstones | Confirmed (all 6) | `remove_voter:1170` — self :1172, leader :1200, quorum `<3` :1203, offline `:1210`, job-owner+tombstone `begin_node_removal:1417`/`finalize:1557`; admin-gated handler `http/cluster.rs:55` |
| 18 | `ClusterAvailability` is pure topology arithmetic | Confirmed | `membership.rs:321–327`, computed from voter count `:1157–1161` |
| 19 | `ReplicationStatus` has no `scope`/`reason`/`confirmed_peer_count` today (PR-C adds them) | Confirmed | `migration.rs:3911–3934`; `voter_count` exists only as an *input* to `ReplicationObservation:3945`, not in the output |
| 20 | Current WAL purge is physical-delete-first, metadata-publish-second | Confirmed | `writer.rs:290` `shift_delete_logs` (physical `remove_file` `wal.rs:977,1000`) precedes `Metadata::write:293–295` |
| 21 | `localhost` advertisement on `nynuc` is a real config path | Confirmed | `configured_advertise_host` fallback `"127.0.0.1"` `migration.rs:1926`; rendered as `"localhost"` `membership.rs:2344–2348` |

Nothing in the plan was **refuted**. The only inaccuracies are cosmetic
(Home line numbers) or a mechanism nuance (§4.2).

## 3. P0 findings — resolve before implementation

### P0-1: the plan duplicates an actively-executing sibling plan it never names

This is the finding that changes the most work. `docs/CLUSTER-PERFORMANCE-PLAN.md`
(CPP) is a live, mid-execution plan in the same repo. Its status line moved
across the 21 post-base commits:

```
base a0f9fc14 : **Status:** ready to build
main 64e96aa7 : **Status:** P0–P2c landed; P2d staged for review
```

The four PRs merged in that window (#516/#517/#518/#519) *are* CPP's P2a→P2d
slices, and they already ship the proof primitives this plan proposes to invent:

| This plan proposes | CPP already merged | Where it lives now |
|---|---|---|
| PR-C: leader-visible peer confirmation / `confirmed_peer_count` | P2d quorum commit watermark — `QuorumWatermarkSample{committed_index, apply_lag_entries}`, `DbQuorumWatermark(term, leader_id, committed_index)` (#519) | `migration.rs` status module + `plurx_raft_commit_index` |
| PR-C: local vs cluster replication scope, apply-lag reason | P2c passive local-Raft observer — `PassiveRaftSample{current_term, last_applied_index, leader_known, is_leader}` (#518); valid-gated apply-lag (#519) | `migration.rs`; `plurx_raft_apply_lag_entries`, `plurx_raft_metric_sample_valid` |
| PR-A: a cluster measurement harness + schema | P0b `benchmarks/cluster-topology.schema.json` + `crates/plurx-cluster-check/src/topology.rs` (#513); P2f named-runner overhead artifact | committed |
| PR-D: instrument the probe/store path | P2b Store-operation metrics — `StoreOperationMetrics` 9-cell histogram → `plurx_store_operation_seconds` (#517) | `store/mod.rs:48` |

The important nuance, verified rather than assumed: the merged primitives surface
**only on the Prometheus `/metrics` scrape path** (`http/system.rs::render_passive_raft_metrics`). The public `ReplicationStatus`
JSON — the thing PR-C actually needs for the Cluster banner — is **byte-identical
base→main**. So PR-C's *deliverable* (the banner projection) is genuinely still
open, but its *source of truth* should now be the merged watermark/observer, not
a new per-request peer fan-out. PR-C §7.1 already says "do not add a Store read
to the `/metrics` scrape path or a new per-request peer fan-out" — that
constraint is now satisfiable for free by reading the already-merged local
sample, which is a better outcome than the plan imagined.

The genuinely-separate part of this plan is intact: the page-read amplification
subject (the `cluster.page-reads` contract, #510) is **not** owned by CPP, and
CPP's bounded-replica reads (P3) explicitly exclude settings/offline/auth — so
the plan's §1.2 non-goal ("do not serve catalogue or watch state from an
unboundedly stale replica … remains the separate contract in CPP") is accurate.

**Why this is P0 and not a nit:** a plan whose entire virtue is precision cannot
ship a PR-A that stands up a second cluster-measurement harness beside CPP's, or
a PR-C/PR-D that re-derive observer/watermark/store-metrics that landed this
week. That is two sources of truth for cluster health in one repo. **Required
before implementation:** an explicit reconciliation section — for each of PR-A,
PR-C, PR-D, state whether it extends the CPP artifact or supersedes it, and cite
the CPP milestone (P2b/P2c/P2d/P2f) it builds on.

### P0-2: re-pin to current main and re-verify the moved boundaries

The plan pins `a0f9fc14` and says to re-verify — but it should re-pin, because
the interim commits touched files it depends on:

```
git diff --stat a0f9fc14 64e96aa7 -- \
  crates/plurx-core/src/cluster/migration.rs   # +1176  (PR-C's target)
  crates/plurx-core/src/store/hiqlite.rs        #  +757  (the 3 s timeout boundary)
  crates/plurxd/src/http/mod.rs                 #        (the router)
  docs/CLUSTER-PERFORMANCE-PLAN.md              #   +46  (status flip)
```

`migration.rs` grew by ~1,200 lines of exactly the observer/watermark code from
P0-1. PR-C cannot be scoped honestly against the old `migration.rs`. Re-pinning
also refreshes the Home line numbers (§2, claim 5) so PR-F's source map is right.

### P0 (recovery): OP-0 4→3 removal is safe *by the code*, but blocked on a live-quorum fact only the operator has

The brief asks whether the normal remove-and-rejoin path is safe in this state.
The removal machinery is sound: `remove_voter` (`membership.rs:1170`) enforces
all six fences the plan lists (§2, claim 17) — it refuses leader removal (:1200),
refuses dropping below three voters (:1203), settles offline work before commit
(:1210), and tombstones the identity durably via trigger (`:110–119`). So the
supported path will not *silently* do the wrong thing; a bad precondition returns
a typed refusal, not corruption.

The real gate is the one the plan already names as a stop condition: removal
itself needs a functioning quorum, and `nuc4` is one of four voters, so the
surviving three *are* the exact quorum of the current config — a second fault
during removal halts it. That is a live-cluster fact (are the other three
converging and answering `/readyz` twice, ten seconds apart?) that cannot be
read from source. **The plan's OP-0 preflight (§5.1) asks for precisely this
evidence before touching `nuc4`, and its stop conditions are correct.** No
change needed to OP-0 except that it must run against a build that includes the
merged quorum-watermark metrics (P0-2) so the preflight can read apply-lag
truthfully instead of inferring it.

One caveat on §5.3: the removal targets a *surviving* node by `node_id`, and the
offline-work settle (`settle_offline_work`) runs inside `remove_voter` before the
membership commit. If the probe loop's replicated read is currently failing (the
brief's 218 failures/2 min), confirm that `settle_offline_work` does not itself
depend on that same failing path — otherwise removal can wedge on offline-work
resolution. This is answerable in code before OP-0 and should be a PR-B/§5
preflight line item.

### P0 (correctness): the WAL crash-window hypothesis is real and un-patched — with one honest caveat

This is the highest-stakes technical claim in the plan, and it holds. The
current purge ordering is physical-first (verified directly in `writer.rs`):

```
Action::Remove (writer.rs:263–306)
  ├─ 274–286  flush active header first  (guards the APPEND-side hole)
  ├─ 290      shift_delete_logs(...)     ── physical remove_file (wal.rs:977,1000)
  │              ⨯ crash here ⨯
  └─ 293–295  Metadata::write            ── last_purged_log_id published LAST
```

A crash between `:290` and `:295` leaves the physical floor raised while
`last_purged_log_id` stays stale-low. The existing reconstruction patch does
**not** cover this: its guard fires only when the boundary is *absent* —
`writer.rs:107` `if meta.read()?.last_purged_log_id.is_none() && … front.id_from > 2`.
After the first purge the value is non-`None`, so a repeat-purge crash slips
past it. On restart, `get_log_state` returns the stale boundary verbatim
(`reader.rs:168`, no clamping), OpenRaft believes purged logs still exist, asks
for one, the reader returns short, and OpenRaft raises exactly
`LogIndexNotFound { want: …, got: None }`. The symptom shape matches.

The plan's proposed metadata-first reorder is a genuine improvement, not a no-op:
it converts the failure mode from "over-claim → missing log" into "under-claim →
extra WAL bytes below the boundary," which the reader already tolerates
(`Action::Logs` skips whole files below the requested `from`, `reader.rs:78–84`).
And it is cheap — `Metadata::write` is *already* a staged, fsync'd,
atomic-rename-plus-dir-fsync write (`metadata.rs:77–113`), so the change is
moving one call above another.

Two things the plan must add:

- **The reorder must preserve the pre-removal active-header flush** at
  `writer.rs:274–286`. That flush exists to prevent a *different* hole (between
  snapshot and the latest appended log) and its comment says so. The §3.2
  sequence diagram omits it; PR-B must keep it, ordered before the physical
  delete, independent of where `Metadata::write` moves.
- **Source-read proves the window exists; it does not prove this window caused
  *this* incident.** The same `LogIndexNotFound` is also consistent with a
  snapshot-install transfer defect or a reconstruction miscompute. Notably, the
  snapshot-install case is the one the reconstruction patch *does* handle — which
  argues *for* the plan's stale-metadata window and *against* snapshot-install as
  the cause, but does not settle it. Closing the loop needs the on-disk
  `meta.hql` `last_purged_log_id` versus the retained WAL floor from the
  preserved `nuc4` copy. The plan's PR-B §6.2 builds exactly that inspector and
  runs it against the forensic copy *before* committing any repair. Keep that
  ordering; do not let the reorder merge ahead of the inspector's verdict.

The vendored WAL is byte-identical base→main, so this analysis survives the
re-pin.

### P0 (correctness): the health surface confuses heartbeat with Raft health — confirmed, and now cheaply fixable

`reachable` is a 30 s heartbeat window (`membership.rs:35,1153`) with no Raft
input, while `ClusterAvailability` is pure voter-count arithmetic
(`:321–327,1157–1161`). So the UI can show `nuc4` "reachable" and the cluster
"HighAvailability" while `nuc4`'s Raft core rejects every append — exactly the
brief's observation. PR-C's fix (separate heartbeat from replication proof, label
the column "Heartbeat") is the right shape. Post-P0-1, its `confirmed_peer_count`
and apply-lag can be sourced from the merged quorum watermark rather than
invented — which is what makes PR-C smaller than the plan currently scopes it.

## 4. P1 latency findings

### 4.1 The auth-read cardinality reduction is sound and correctly targeted — state it affirmatively

The plan's central latency lever is real and the code backs it precisely. Every
authenticated request pays one leader-confirmed consistent read before its
handler runs (§2, claims 1–2), and Home currently issues `3 + L` such requests:

```
current Home:  /libraries ┐
               /hubs       ├─ 3 top-level      each → 1 consistent auth read
               /coming-soon┘                    (query_consistent_map, leader-confirmed)
               + L × /libraries/{id}/items      each → 1 consistent auth read
               ────────────────────────────────────────────────────────────
               = 3 + L leader-confirmed reads, each capped at the 3 s STORE_TIMEOUT

proposed:      /hubs · /home/previews · /coming-soon  = 3, constant in L
```

Under a voter that makes leader read-confirmation time out, every one of those
`3 + L` reads can absorb up to 3 s — which is why the observed Home time pins to
~3.2 s (the `STORE_TIMEOUT`). Collapsing the fan-out to a constant three, each
paying one auth decision, attacks the mechanism directly, and the windowed
`home_preview_pages` query that backs it is character-for-character faithful to
the existing Added-sort semantics (§2, claim 12). This is the strongest part of
the plan and it should be called out as such.

### 4.2 The Home mechanism description is slightly wrong — the fix is unaffected

The brief (§3, "Home waits for both request waves") says previews start "after
`/libraries` returns." In fact previews start at `web/index.html:3630`, *after
the entire first `Promise.all` resolves* — so a slow (non-failing) `/hubs` or
`/coming-soon` also delays the preview wave, not just `/libraries`. This makes
the all-or-nothing coupling slightly worse than the brief states, and it
strengthens PR-F's case for committing `/hubs` as first content independently.
Correct the sentence; no code consequence.

### 4.3 Settings and Activity findings are accurate; the remedies are right

Settings genuinely awaits seven endpoints before any tab paints (§2, claim 8),
so opening Libraries really does wait on `/system`, `/users`, `/trakt/status`,
and up to 2,000 playback events — a per-tab dependency manifest (§3.5) is the
correct fix and the existing `page-read-budget.test.js` already asserts the
seven-endpoint set, so the test will move with the change and catch regressions.
Activity's all-or-nothing body (§2, claim 10) and its 3 s poll are confirmed;
retaining the last body during refresh (§3.6) is right. The generation fence PR-F
must preserve already exists and is already the single cancellation authority
(§2, claim 11) — PR-F should extend it, not replace it.

### 4.4 PR-F's phase machinery is genuinely new

`shell`/`content`/`settled` phases on `#main.dataset.phase` and `setPagePhase`
do **not** exist today (grep is empty; the only `shell` token is an unrelated
layout-region key). PR-F is building this from scratch — fine, but scope it as
new surface, and note the structural UI golden (`ui-structure.golden`,
`make ui-check`) will change deliberately, as §8.2 says.

## 5. P1 validation findings

### 5.1 The plan is right that PR #510's gate never measured the user outcome

`validation/points.toml:388–404` `cluster.page-reads` states, verbatim, that the
gate "measures the Settings and Activity Store primitives as attempted Hiqlite
client-call families, **not complete HTTP pages or network RTTs**." The web
behavior is a *separate* contract (`web.experience` →
`regressions.d/9a0ebf1e-web-experience.toml`, checked by `web-static`). So the
brief's framing — "the tests didn't lie; the enforced contract was narrower than
the user-facing claim" — is exactly correct, and both regression files it cites
exist. PR-A's page-outcome artifact is the right thing to add on top; it is not
redundant with the Store-primitive gate.

### 5.2 PR-A's new files land under audited paths and need points entries

`docs/**` is **not** in `settings.audit_paths`, so the plan/review/this-review
docs need no `points.toml` entry. But `scripts/**` and `benchmarks/**` **are**
audited — so PR-A's `scripts/cluster-page-latency` and
`benchmarks/cluster-page-latency.schema.json` each require a functionality point
or `make validation-lint` fails with "audited file has no functionality point."
PR-A §6.1 already promises "a validation point mapping the harness, schema … and
tests," so the plan is aware; this is a confirmation, not a gap. Post-P0-1, that
point should extend CPP's existing harness point rather than create a parallel
one.

### 5.3 The commit-history gate will bite a carelessly-titled doc commit

`validation/history.py`'s `ISSUE_RE` (`:21–35`, case-insensitive) flags any
commit subject containing words like `fix`/`correct`/`restore`/`prevent`/
`bound`/`repair`/`recover`. A doc commit whose subject trips it needs an
`ignore = true` self-entry in `regressions.d/` or `make history-check` fails —
this is why the plan's own commits ("docs: add cluster page latency …") passed:
"add" is not a trigger. Actionable for whoever lands PR-B/PR-C ("fix"/"repair"
in a subject will trip the gate) and for delivering this review (see §9).

## 6. P2 follow-ups

- **`localhost` advertisement is a real path, correctly demoted.** The advertised
  cluster address falls back to the literal `"127.0.0.1"`
  (`migration.rs:1926`) and renders as `"localhost"`
  (`membership.rs:2344–2348`) — so `nynuc`'s display is a real config artifact,
  not a rendering bug. The brief is right to call it "a review item, not a proven
  cause." Worth a P2 to verify `nynuc`'s committed `raft_address`/`api_address`
  are LAN-routable, since a loopback advertisement on a real voter *would* be a
  quorum-path problem — just not the one causing this incident.
- **Probe-loop backoff (PR-D) is still warranted, but its observability is
  already half-built.** The 500 ms warn-storm with no backoff/dedup is confirmed
  (§2, claim 16). PR-D's backoff+dedup is a good change; but the plan should note
  that the *rate* it complains about is now measurable via the merged
  Store-operation metrics (P0-1), so PR-D can add a gauge cheaply instead of
  inferring the storm from logs.
- **Connection churn (P2 in the brief) is unverifiable here and correctly hedged.**
  The "many accepted Hiqlite WebSocket streams" observation is a live-fleet log
  fact; the brief already marks it "plausible amplifier, not yet a proven primary
  cause" and says to instrument before changing pooling. Agreed — no source claim
  to check.

## 7. Answers to the plan's §11 reviewer decisions

1. **Live recovery (4→3):** Safe by the code (all fences present), but gated on a
   live-quorum fact only OP-0 preflight can supply. Additional evidence to
   capture first: confirm `settle_offline_work` does not depend on the same
   replicated-read path that is currently failing (§3, P0-recovery). Run OP-0
   against a build carrying the merged quorum-watermark metrics so apply-lag is
   read, not inferred.
2. **WAL candidate (metadata-first):** Endorsed. It satisfies OpenRaft 0.9's
   contract because it can only leave *extra* bytes below the purge boundary,
   which the reader skips (`reader.rs:78–84`). Two conditions: keep the
   pre-removal header flush (`writer.rs:274–286`), and land the PR-B inspector's
   verdict on the forensic copy before the repair (§3, P0-correctness/WAL).
3. **Forensic scope (§3.1 fields):** Sufficient to distinguish the four classes,
   with one addition — record whether `last_purged_log_id` is `None` vs
   stale-non-`None`, because that single bit separates the snapshot-install case
   (reconstruction-covered) from the repeat-purge crash window (uncovered), which
   is the whole question.
4. **Health contract (`scope`/`reason`/`voter_count`/`confirmed_peer_count`):**
   The smallest truthful set — but source `confirmed_peer_count` and apply-lag
   from the already-merged quorum watermark / passive observer (P0-1), not a new
   fan-out. `voter_count` already exists internally (`ReplicationObservation:3945`);
   promoting it to the output is fine.
5. **Home endpoint:** Yes — the windowed primitive preserves every current
   top-level and ordering rule (§2, claim 12, verified against both backends and
   in sqlite3). Keep the limit fixed at 24; a caller-selectable limit widens the
   Store-call gate's surface for no page benefit.
6. **Settings dependencies (§3.5 manifest):** The manifest matches the seven
   endpoints and their tab affinity as coded. Re-verify at implementation that no
   *mutation* handler reads a field outside it; the read set is accurate today.
7. **Latency budgets (§1.1):** Ratify with named-runner variance, not from the
   sandbox — these are LAN wall-clock claims and, as the plan says, GitHub CI
   cannot honestly prove them. Keep the percentile gate; the p50/p95/max shape is
   right and means-hiding-pauses is a real trap.
8. **PR boundaries:** The one boundary to revisit is PR-A/PR-C/PR-D against CPP
   (P0-1) — after reconciliation, they shrink. No milestone is too large to
   review as scoped; the "one rollback boundary per PR" discipline is correct.
9. **Mobile boundary:** Confirmed — the additive `/home/previews` endpoint and
   the unchanged existing contracts require regression testing (`make apple-test`
   / `make android-test`) but no native feature work. Apple/Android never call the
   new route.

## 8. Destructive / availability-affecting operations — decision table

| Operation | Prerequisites | Blast radius | Rollback | Approval |
|---|---|---|---|---|
| Preserve forensic copy of stopped `nuc4` | `nuc4` process + data-dir lock gone | none (read/copy only) | n/a | operator |
| `DELETE …/nodes/{nuc4_id}` (4→3) | 3 non-target voters ready twice @10 s; target not leader; offline work quiesced; `settle_offline_work` path healthy | membership change; below 3 voters a second fault halts progress | pre-commit: restart unchanged voter · post-commit: identity tombstoned, only a fresh join recovers capacity | explicit operator |
| WAL metadata-first reorder (PR-B) | inspector verdict on forensic copy; pre-removal header flush preserved; on-disk format unchanged | vendored WAL write path on every node it deploys to | preceding binary reopens a healthy voter (confirm with restart test) | reviewer + operator |
| `/home/previews` + Store primitive (PR-E) | Store-call gate cardinality-independent; parity on both backends | additive endpoint; no client uses it until PR-F | revert directly (unused) | standard PR |
| Web hydration rewrite (PR-F) | PR-E deployed; generation fence preserved; UI golden reviewed | embedded web app behavior | deploy preceding binary; additive endpoint harmless | standard PR |
| Rolling deploy (REL-1) | `/readyz` + applied-index catch-up + attributable build per voter | one voter at a time; never two down on a 3-voter cluster | roll back code one voter at a time; never restart a tombstoned identity | operator |

## 9. What could not be verified here, and how this review was delivered

- **Operator-supplied, not container-reproducible:** the live log counts (1,014
  read-confirmation failures, 2,071 `LogIndexNotFound`, 218 probe failures / 2
  min), the per-route timings (761 ms / 3.2 s), the fleet build stamps, and the
  `nuc4` on-disk WAL state. These are diagnoses from Paul's running cluster; they
  are internally consistent and consistent with the code paths, and they are
  left to stand on their own evidence rather than converted into tasks. The one
  measurement that would upgrade the root cause from strong-inference to fact —
  per-operation Store timing under the broken voter — is what PR-A is built to
  capture.
- **Not built in the sandbox:** the full workspace was not compiled or a cluster
  spun up for this review; findings are source-verified at `a0f9fc14` (and
  diffed against `64e96aa7`), and the proposed Home SQL was executed against
  sqlite 3.45.1 to confirm it parses and ranks correctly. Building plurxd and
  running `make cluster-check` is the right next gate when PR-B/PR-E land — that
  is implementation verification, not review verification.
- This review is delivered as `docs/CLUSTER_PAGE_LATENCY_FIX_PLAN_REVIEW.md` on
  `codex/cluster-page-latency-review`, committed with a subject that avoids
  `ISSUE_RE` (§5.3) so `make history-check` stays green with no ledger entry.
