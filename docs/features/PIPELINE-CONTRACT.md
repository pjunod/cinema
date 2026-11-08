# Pipeline contract v2 — one terminal fact per job, one owner per boundary

**Status:** built — the cinema slice (§4–§6) is built on `effort/pipeline-v2`,
not yet promoted to `main`, and documented in [INTEGRATION.md](../INTEGRATION.md)
§1–§2; the approved design was revised after three adversarial review rounds
(r4) · **Supersedes:** curator `monarr/docs/plan-integration.md` §3 (contract),
`monarr/docs/DOWNLOAD_LIFECYCLE_PROPOSAL.md` §4.1–4.2 (notification/acknowledgement),
runner `nzbd/docs/CURATOR_DELIVERY.md` (acknowledgement protocol), the "Durable job
control contract (v1)" section of curator `monarr/docs/integration.md` · **Decided:**
2026-11-07 (Paul: rewrite the seam; keep the manifest; a post-processing
failure is a failure unless the job itself is marked keep-on-failure) ·
**Written:** 2026-11-07 · **Revised:** r2 after the agent review, r3 and r4
after Astra's two rounds, all 2026-11-07 (log in
`PIPELINE_REVIEW_LOG.md` (project doc))

Companion to `PIPELINE_AUDIT.md` (project doc) (what is broken and why) and
`PIPELINE_REWRITE_PLAN.md` (project doc) (how it gets built) — this
is *the seam itself*: what each application promises the other two, in wire
terms. When the code and this document disagree during the rewrite, this
document wins; after the rewrite ships, the per-repo `INTEGRATION.md` files
are rewritten from it and this file becomes the archive.

The whole contract fits in three sentences. **Runner emits exactly one terminal
fact per job, before it does anything else with the bytes.** **Curator drives
its download row from runner's event stream and uses the poll only to catch
what the stream missed.** **Curator tells cinema what landed through a durable
outbox and reads back whether cinema indexed it.** Everything below is the
consequence of those three.

**This repository's copy.** The canonical contract is the cross-repository
project document `PIPELINE_CONTRACT.md`; this copy is what plurx builds
against. Cinema's slice is §4 (what Curator sends and reads back), §5 (what
plurx promises) and §6 (rollout). The cinema implementation is tracked in
[the status page](../../STATUS.md) under *Pipeline contract v2 (cinema
slice)*, on `effort/pipeline-v2`. Line references to `scan.rs`,
`library_work.rs`, `scan/mod.rs` and `background_jobs*.rs` are to plurx
`main` at 2026-11-07; references to `manager.rs`, `deliver.go`, `plurx.go`
and similar are to Runner and Curator.

---

## 1. Vocabulary — the words both sides use for the same thing

| Word | Means | Not |
|---|---|---|
| **job** | one runner download attempt, `JobId` | a release, a title |
| **transfer** | curator's request token — an **opaque string compared by exact equality** (today `t-<rowID or 0>-<32 hex>`, 16 random bytes, minted before the row may have an id, `ports/transfer.go:12`; deployed values must be preserved as-is), runner job param `monarr-transfer`, cinema `correlation_id`. **Every fact about a job carries it, and a fact whose transfer does not match the row is dropped** (§3.2) | a parseable structure; a row id |
| **terminal** | `completed`, `failed` or `cancelled`, **authored by runner**. Emitted once, never revised. A terminal curator writes on its own (`submit`, `lost`, `stalled`) is *provisional* and yields to any later runner fact (§3.2) | held, paused, retrying |
| **held** | runner is waiting on something *environmental* that will come back (storage, space, a password) with a deadline after which **runner** ends it as `failed/hold_expired` | a post-processing error; something curator decides |
| **publication** | a directory under `<completed>/` named `<name>.job-<id>.runner-<gen>` containing only selected files **and** `.runner-manifest.json`, written last as the commit marker. A directory without the manifest is not a publication — curator ignores it, runner's restart reconciliation finishes or unwinds it (§2.5). **During the rollout a v1 delivery record is an equivalent manifest** (§6) | runner's processing dir |
| **publication generation** | `<gen>` — the unit curator imports, notifies and cleans up. One download row may have several (the completed one, a salvage, a late one); each has its own lifecycle (§3.4) | the download row |
| **manifest** | the immutable file list + per-file size + SHA-256 + generation | an acknowledgement |
| **owner** | the one application allowed to delete under a path | — |

Ownership, unchanged from the Oct 22 decision and now enforced on both sides:

| Path | Owner | Exit |
|---|---|---|
| runner `/processing/<job>` | runner | publication or terminal failure → runner disposes per §2.6, **after** any salvage obligation settles |
| runner `/processing/failed/<job>` | runner | retention or explicit delete; **never gates curator** |
| `<completed>/<publication>` | curator, from the moment the manifest exists | **per file**: a file leaves when its placement is recorded or it is superseded; the directory leaves when it is empty (§3.4). Curator never asks runner to delete it |
| library | curator | library operations only |

---

## 2–3. Runner → curator, and curator's reducer

Sections 2 and 3 — runner's terminal fact, failure reasons, holds, status,
publication and the manifest, and curator's states, reducer, failure→next
grab, per-generation import and overdue predicate — do not touch plurx
and are kept, verbatim and with the same numbering, in
[PIPELINE-CONTRACT-RUNNER-CURATOR.md](PIPELINE-CONTRACT-RUNNER-CURATOR.md),
so this page stays inside the docs size budget. A `§2.x` or `§3.x`
reference below points there.

---

## 4. Curator → cinema

### 4.1 Outbox, not bus — one sub-delivery per directory

`ImportCompleted` no longer travels over the in-memory bus (`bus.go:59`,
32 slots, drop on full). In the publication's completing transaction, **one
`notifier_deliveries` row per `(notifier, publication, directory)`** is
inserted with a stable idempotency key
`<transfer>/<generation>/<dir-hash>` and the list of media paths the import
placed under that directory. The dispatcher drains rows independently — the
20 s dispatcher deadline (`deliver.go:130`) applies **per sub-delivery**, so
three slow directories cannot starve each other or repeatedly re-post the
first two. Backoff `5 s → 30 s → 2 m → 5 m → 15 m → 1 h`, then hourly,
**without end** for transient failures; permanent only on `401`/`403` (key),
`400` (body shape) and `422 path_outside_roots`. `429`, `503
path_unreadable`, connection errors and timeouts are transient. Today the
queue gives up after `5s, 30s, 2m` (`deliver.go:30-34`) and classifies `429`
as permanent (`plurx.go:196`).

### 4.2 The request, and reading the answer

Body unchanged (`path · ids|series|book · hint · correlation_id · source`).
Four additions:

- `"request_key": "<idempotency key>/<attempt>"` — cinema returns the
  **same** `request_id` and result for a repeated key (§5.1), so a lost
  response does not create a second scan request. The sub-delivery carries
  an **attempt number**: the key is reused after an *ambiguous* outcome (a
  timeout, a connection error, no response) and **advanced** after a
  *confirmed retryable* result (a `scanned` result that did not place the
  expected file, a `503 path_unreadable`, a `failed` request), so a retry
  after the file has been restored or the share remounted gets a fresh scan
  instead of the cached skip.
- `"expected_files": [{"path": …, "bytes": …, "mtime_ns": …}]` — the media
  files this import placed under `path`. Cinema **re-probes these paths
  regardless of its size+mtime cache** (`scan/mod.rs:1481` skips the probe
  when both match an existing record, which cannot tell a same-path
  replacement with equal size and whole-second mtime from the old file) and
  reports each as `placed{item_id, bytes, mtime_ns}`, `skipped{reason}` or
  `missing`.
- `"wait_ms": 10000` — cinema may hold the request up to this long and answer
  `200 {"status":"scanned", "items":[…], "files":[…]}` if the targeted scan
  finishes; otherwise `202 {"status":"queued","request_id":…}` as today.
- On `202`, curator **polls** `GET /api/v1/scan/requests/{request_id}` on the
  outbox schedule until `scanned` or `failed` (bounded: 30 min, then the
  sub-delivery is `warning` with cinema's `waiting_on`, not `failed`). The
  `scan:trigger` scope is allowed to read **requests it created** (matched by
  key id, stored on the request).

A sub-delivery is **`visible`** only when every expected file comes back
`placed` with matching `bytes` and `mtime_ns`. A `scanned` result that
indexed an old sibling and skipped or missed the new file is `warning:
expected <path> not placed (<cinema's reason>)`; the attempt advances, it is
retried on the schedule and surfaced after 30 min. The delivery log row shows
`visible → plurx item <id>` with the real item id, or the exact `waiting_on`
/ skip text from cinema.

### 4.3 Path check at setup

The notifier's **Test** sends the configured library roots (from Settings →
Library) as `"probe_paths"` on the connection-test request; cinema answers
which it can `stat` (§5.3). A root cinema cannot see fails the Test with the
two paths side by side. On an old cinema, `path` is required
(`scan.rs:44-47`) and the pinned Axum JSON extractor rejects the missing
field with a **`422`** whose body is the extractor's own message — not the
handler's `{"error": "path is not under any library root", "roots": […]}`
422. The Test distinguishes them **by body** (`roots` present ⇒ the handler
spoke; absent ⇒ the extractor did), falls back to today's
`/monarr/connection-test` probe on the extractor case, and reports that path
checking needs a newer cinema; the compat test exercises the real old
handler, not a stub. Today the mismatch is found one import at a time, as a
permanent 422.

---

## 5. Cinema

### 5.1 `POST /api/v1/scan` and the targeted job

- Accepts `request_key`; a repeated key returns the existing request
  (`200`/`202` with the same `request_id` and result) — the request record
  carries the key and the creating key's id. A key with a new attempt suffix
  is a new request.
- Accepts `expected_files`; those paths are **probed unconditionally** (the
  size+mtime short-circuit at `scan/mod.rs:1481` is bypassed for them) and
  reported per file as `placed{item_id, bytes, mtime_ns}` / `skipped{reason}`
  / `missing`. Other files under `path` are scanned as today.
- Honours `wait_ms` (cap 15 s): wait on the request's completion notify, then
  answer 200 or 202. `200` includes `files` (the per-file report above) and
  `items`.
- **Typed path outcomes.** Containment is decided **lexically first**: a
  request path that is not under any configured root (after normalising `..`
  and symlink-free prefix matching) is `422 {"code":"path_outside_roots"}`,
  permanent. A path under a root that cannot be canonicalised or `stat`ed
  right now (unmounted share, `EIO`, `ENOTCONN`) is `503
  {"code":"path_unreadable", "retry_after_s": 60}`, transient. Today both are
  the same 422 (`scan.rs:113-125, 279-280`), so an unmounted share discards a
  valid notification permanently.
- `GET /api/v1/scan/requests/{id}` readable by the creating key and returns
  `waiting_on` while queued: `full_scan:<library>` · `heavy_lane:<holder>` ·
  `root_unreadable:<path>` · `queue_position:<n>`.
- **Targeted requests are their own job kind**, `library:targeted:{library_id}`,
  separate from the full-scan job they share today
  (`library_work.rs:292-387` coalesces both into one claimed job). The
  targeted job claims with a **probe admission** — `stat` + `ffprobe` on the
  named files, no `transcode.admit_fragment()` (`background_jobs.rs:2700`) —
  and runs **ahead of a *pending* full scan** of the same library. Against a
  **running** full scan it does not run concurrently: both job kinds take
  the **per-library mutation fence** (`background_jobs_domain.rs:30-32`), so
  the full scan's final known-minus-seen reconciliation (`scan/mod.rs:887-932`)
  cannot prune an item a concurrent targeted scan just inserted; the targeted
  request waits with `waiting_on: full_scan:<library>` and runs the moment
  the fence is released. *Built note (decision on #1405, 2026-11-08):* a
  probe of at most 64 named files takes **no background source reader**, so
  the per-library fence is the targeted job's only wait. `full_scan:<library>`
  names only that library's own running scan; a full scan of another library
  on the same volume does not hold it, and `heavy_lane:<holder>` is never
  written for a targeted job. No preemption in this round; a safe yield point
  (between directories) is a later improvement if full scans prove long
  enough to matter. Fragment index, DV title scan and cache work for the new
  item stay on the heavy lane, as today. This is a store change (`key_id`,
  `request_key`, `waiting_on`, `placed_paths` on the request record, in both
  the SQLite and the hiqlite catalogues), not a flag.
- The UI's Jobs panel lists a waiting targeted request with its `waiting_on`,
  and a Cancel.

### 5.2 Reconcile

`scan_interval_mins` default becomes `360` for **new** libraries. Existing
libraries at `0` are preserved; Settings offers a one-click opt-in to 360 on
each and a one-line notice reading: "Never: a missed notification is found
only by a manual scan, or at startup if *Scan libraries at startup* is on
(it is off by default)." *(Decision for Paul to look over: both reviewers
recommend preserve-and-opt-in over a silent migration.)*

### 5.3 Connection test

`POST /api/v1/scan` with `"probe_paths": [...]` and no `path` answers
`200 {"roots": [...], "visible": [...], "missing": [...]}`; needs
`scan:trigger`. Probe paths are accepted only when each is a configured
library root or a descendant of one; anything else is reported as
`outside_roots`, never `stat`ed.

### 5.4 Not in this contract

The mark-bad / replace API (`BAD_DOWNLOAD_API.md`) is unchanged: a draft, not
implemented on either side, and out of scope until the three goals above hold
in production for two weeks.

---

## 6. Compatibility and rollout order

Each application must be deployable one at a time; the matrix below is what
makes that true and is tested in each repo's compat suite.

**The v1 adapter.** Until runner's M3 ships, publications carry no
`.runner-manifest.json`; they carry a v1 delivery record (`/deliveries/{id}`
with its manifest). Curator's importer takes its manifest from **either**
source through one adapter (`publicationManifest(dir) → manifest`), preferring
the file, so a `completed` terminal from an M1 runner imports through the
new path, the 232 publications already on lab3 import through the new path,
and a curator rollback to the pre-M2 build still sees the v1 record it
expects. Runner keeps writing the v1 delivery record for one release after
M3 for the same reason.

**Unfinished v1 work migrates, it is not dropped.** The old curator
acknowledges a delivery *before* placing its files (`delivery.go:679`); a
crash after the ack leaves an accepted publication with no library copy,
which runner's default discovery omits (`delivery.rs:568`) and whose local
retry the M2 build no longer schedules. So the M2 migration converts every
local v1 receipt (`runner_deliveries` and its per-file journal) into a
`publications` row with the per-file outcomes it had reached (`placed`
where a placement was journaled, otherwise `pending`), cohort `historical`,
and curator reads `/deliveries` with `include_accepted=true` from then on.
An accepted-but-unplaced publication therefore imports through the new path
on the first poll after upgrade. Tested with an advanced cursor and an
accepted, partially placed publication.

| Runner | Curator | Works because |
|---|---|---|
| old | new | curator's reducer accepts the legacy `job_pp_finished` (`pp_status` prefix → outcome, `blames_release` inferred as today's blocklist rule, `salvage: none`); holds without `deadline_in_s` get `held (stale)` after 24 h; the v1 adapter imports from the delivery record; `accepted` rows are ignored |
| new | old | `pp_status`/`final_dir` are still on the event; `/request-admissions` still answers; `accept` answers `200` no-op so old curator's ack settles. **Known gap for this window:** new runner fails what old curator still parks (audit C1/C4) — every PP failure in the window is a one-way park, so the curator milestone deploys the same day (plan §5) |
| new | new | the contract |

| Curator | Cinema | Works because |
|---|---|---|
| new | old | `wait_ms`, `request_key` and `probe_paths` are ignored by old cinema; 202 is handled; the status poll gets 403 and the sub-delivery shows `queued (cinema does not report results)`; the Test falls back to the legacy probe (§4.3); old cinema's single 422 is treated as permanent as today |
| old | new | nothing changes for old curator |

Order: runner M1 → curator M2 (same day) → runner M3 → curator M4 → curator
M5 + cinema M5 → M6 deletions (see the plan). Nothing is feature-gated in
code; where an enable is needed it is advisory in the Developer tab.

---

## 7. Non-goals

- **No new transport.** SSE + REST + SQLite, as today. No broker, no gRPC.
- **No curator → runner cleanup commands.** Curator never deletes under
  `/processing`; runner never deletes under `<completed>`.
- **No title matching across any seam.** Transfer token and ids only.
- **No per-symptom watchdogs.** A stuck state is fixed by giving it a
  deadline in the state machine, not by a sweeper that looks for it.
- **No new caps.** The two that remain (bytes budget, 20/12 h) are the only
  two.
- **No curator-authored final terminals.** Curator may give up waiting; only
  runner may say what happened.
- **No mark-bad API** in this round (§5.4).
- **No backlog mutation** by the rewrite. The 1.99 TB on lab3 is handled by
  the plan's M7 as a read-only report and operator-approved batches under the
  same per-file evidence rule as §3.4.
