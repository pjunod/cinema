# Pipeline contract v2 — §2–§3: Runner → curator, and curator's reducer

**Status:** approved design (r4), not built · **Written:** 2026-11-07

Companion to [PIPELINE-CONTRACT.md](PIPELINE-CONTRACT.md) (the contract's
head, vocabulary, the Curator → plurx seam, plurx's promises, rollout and
non-goals) — this is *the two sections of the same contract that are between
Runner and Curator*, split out verbatim only so each page fits the docs size
budget. Section numbers are the contract's own. plurx implements none of
this; it is here so a reference from §4–§6 resolves.

---

## 2. Runner → curator

### 2.1 Terminal outcome — `job_pp_finished`, once, first, typed at the source

The existing event keeps its name (every consumer already subscribes to it)
and gains a closed outcome. It is emitted **exactly once per job**, **after
the history row is durable and before any disposal, cleanup, salvage or
retention work** on the job's bytes. Nothing that happens to the bytes
afterwards changes the outcome. Two later, non-terminal facts may follow it:
`job_publication_ready` (§2.2, salvage) and the disposal state on the
artifact inventory (§2.6).

```json
{
  "event": "job_pp_finished",
  "job": 2333,
  "name": "Dry.Wind.2020…",
  "category": "movies",
  "transfer": "t-591-a3f9c1",            // == params[monarr-transfer]; absent only for jobs curator did not add
  "outcome": "completed",                 // completed | failed | cancelled
  "reason": null,                         // failed: closed enum, §2.2
  "stage": null,                          // failed: PostStage or "download"|"health"|"fetch"
  "message": null,                        // failed: ≤ 2 KiB human text
  "blames_release": false,                // failed: whether curator should blocklist the release (§2.2)
  "salvage": "none",                      // failed: none | pending — runner will assess and publish usable partial output
  "keep_on_failure": false,               // the job's own flag, echoed
  "fetched": {"success_articles": 1204, "total_articles": 1204, "bytes": 8123456789},
  "publication": {                        // completed only; null on failed (salvage arrives separately)
    "path": "/working/monarr/completed/Dry.Wind…job-2333-url-fabba0b486c9.runner-29c8…",
    "generation": "29c89841a6c050b662dc794671e52708",
    "manifest_digest": "a0acee1d…",        // the deployed runner-manifest-v1 digest, bare hex (IMPL_RUNNER §3.6)
    "files": 2,
    "cross_volume": false                 // true when the payload was copied, not renamed (§2.5)
  },
  "pp_status": "SUCCESS",                 // kept for one release: the legacy history string
  "final_dir": "…",                       // kept for one release: == publication.path
  "params": [["monarr-transfer","t-591-a3f9c1"], …],
  "history_seq": 4412,                    // never 0: the row is written or the event is not sent
  "history_incarnation": "7f3a…"          // the history index's identity (§2.5)
}
```

Rules, each with its reason:

- **One authoritative terminal record, then everything else replays from
  it.** The terminal is first written as a single row in the state database
  (`job_terminals`: job, transfer, outcome, reason, stage, blame, salvage
  obligation, the queue markers to stamp, `emitted_at = NULL`) in **one
  SQLite transaction with the artifact's salvage obligation**. The history
  append (its own durable write, `history.rs:710`), the queue-row markers
  and the SSE emit are then performed *from* that record and marked done on
  it; on boot, any `job_terminals` row with an unfinished step is replayed
  before the post manager, the hold-recovery task or disposal run. Disposal
  checks the record, not the history row. A crash between any two of those
  writes therefore cannot expose a failure in history while leaving disposal
  without its salvage obligation, or stamp the queue without a history row.
- **Write-then-emit, and the write is part of the job.** If the terminal
  record or the history write fails, the job is held `storage_unavailable`
  on stage `persistence` (an environmental hold, §2.3) and retried; the event
  is sent when the write lands. Today the event path has `history_seq: 0`
  documented and unreachable, and every emit site returns early on a write
  failure (`manager.rs:4912, :1551`) — the consumer is left with nothing.
- **Terminal before disposal and before salvage.** Today `FAILURE/HEALTH` is
  emitted only after `handle_failed_job_admitted` (930 lines, 36 silent
  exits) completes (`manager.rs:730-1663`), and on lab3 the publish step it
  would wait on is median 98 s, p90 14 min. A consumer that needs to pick the
  next release must not wait on runner deciding what to do with the bytes of
  the last one. Salvage, when it applies, is a **persisted obligation**
  written in the same step as the history row; disposal is not eligible
  until it settles (§2.6).
- **Reasons are typed by the producer that saw the error**, not inferred
  later from a status. Today the owner's `final_status` folds a
  write-failure into the health verdict (`owner.rs:7410`, `queue.rs:1825`),
  so a full disk reads as a bad release and gets the release's disposal
  policy. Each producer names its reason: the owner emits `health` (articles
  missing) or `io` (writes failed), the URL fetcher emits `fetch` from its own
  site (`owner.rs:5287`), the PP manager emits the PP reasons.
- **Health and fetch terminals are emitted by the owner**, at the point where
  `job_finished{failed}` is emitted today (`owner.rs:7478`), with the history
  row written in the same step. They never pass through post-processing
  admission (`BeginPostAdmission`, `owner.rs:2507`), which can defer them for
  a restart re-verification pass.
- **`pp_status` and `final_dir` are carried for one release** so a curator
  that has not yet moved to the reducer (§3) keeps working; they are removed
  in the release after both sides ship.
- **`job_finished` is unchanged and still advisory** (download phase ended;
  PP may follow). Curator ignores it, as today.
- **Torrents:** `job_pp_finished` fires when post-processing ends (payload
  ready), exactly as for Usenet. Seed retirement is a later, separate
  lifecycle on the same job and never produces a second terminal. A torrent
  that stops with a partially verified payload is `failed/io` with
  `salvage: pending` (the verified pieces are salvageable).

### 2.2 Failure reasons — closed, typed, with blame and salvage per reason

| `reason` | `stage` | Set when (by) | `blames_release` | `salvage` may be `pending` | Bytes after (§2.6) |
|---|---|---|---|---|---|
| `health` | `health` | articles missing below critical (owner) | yes | **yes** — the current handler already attempts PAR repair and salvage on this path (`manager.rs:897, 955`); 32 of the last 112 on lab3 fetched *something* | `failure_action`, after salvage settles |
| `fetch` | `fetch` | NZB/URL fetch exhausted (URL fetcher, own site). Carries `fetch_cause`: `gone` (the indexer answered 404/410/403 for *this* item) or `indexer_unavailable` (exhausted through 429/5xx/timeouts/transport, `fetch.rs:230`) | **only when `cause: gone`** — an indexer outage must not blocklist healthy releases; curator re-queues the want after the indexer's `retry_at` instead of grabbing an alternative | no | nothing to dispose |
| `par` | `par_verify`/`par_repair` | repair impossible or budget exhausted (PP) | yes | yes | `failure_action`, after salvage settles |
| `unpack` | `unpack` | extractor failed on the bytes, not the disk (PP) | yes | yes | `failure_action`, after salvage settles |
| `script` | `script` | post script non-zero (PP) | no | yes | `failure_action` |
| `publish` | `move`/`publication_verify` | nothing passes selection: unknown kind, missing proof, empty (PP) | no | yes if anything verified | **`park`, always** |
| `io` | any | write failure, digest/size mismatch, missing file, refused permission (owner or PP) | no | yes | **`park`, always** |
| `identity` | any | the evidence disagrees with itself — conflicting yEnc metadata, foreign journal bytes (owner or PP) | yes | no | **`park`, always** |
| `hold_expired` | the hold's stage | an environmental hold (§2.3) passed its deadline (hold recovery) | no | yes | `park` |
| `cancelled` | — | operator delete / curator cancel; `outcome: cancelled`, **emitted by runner's delete path as a real terminal** (curator never infers it from `job_deleted`, which carries no transfer) | no | — | runner's delete semantics |

**A post-processing failure is terminal.** `HoldCause::PostFailure`,
`HoldCause::Io` and `HoldCause::IdentityConflict` are deleted; the paths that
produced them produce `outcome: failed` with the matching `reason`. The one
exception is the job's own flag: a job added with `keep_on_failure: true` (or
the `monarr-keep-on-failure` param) is still *terminal failed* — the event
fires — but runner retains the source under `/processing/failed` with
`Keep` set instead of disposing it. **Keep changes what happens to the bytes,
never whether the failure is reported.** On lab3, 24 of the last 29 `DELETED`
history rows carry a `held` control — jobs that were deleted while held; a
review hold's only exit.

**`publish`, `io` and `identity` always park, whatever `failure_action` says.**
These are the failures where the bytes are, by construction, intact or
uncertain and the *policy* or the *environment* failed (19 of those 24 held
rows were "no verified media … eligible for publication" on cleanly extracted
releases). Deleting them on a default `failure_action = delete` would turn a
reporting fix into a data-loss regression. Parked sources follow the normal
`failed_retention_days` clock, show their reason and expiry in Files, and
explicit Keep extends them through retention. *(Decision for Paul to look
over: park-always for these three reasons, versus honouring
`failure_action`. Both reviewers recommend park.)*

**`blames_release` decides the blocklist**, not the mere fact of failure. A
local permission fault, a full disk or a selection-policy miss must not
blocklist a good release and spend the re-grab budget across a library;
missing articles, a broken PAR set, a corrupt archive and self-contradicting
post metadata are the release's fault.

**Salvage is a second fact, not part of the terminal.** When `salvage` is
`pending`, runner has persisted a salvage obligation in the same step as the
history row; it then assesses and publishes whatever verified output exists
and announces it with `job_publication_ready {job, transfer, publication}`
(also visible on `/deliveries`, kind `salvage`); if nothing is usable the
obligation settles as `salvage: none` on the artifact. Curator may import a
salvage publication; the *job* is still failed for re-grab purposes. "Exactly
one terminal per job" holds; "exactly one event per job" was never the
promise.

### 2.3 Holds — environmental only, visible, with a deadline runner enforces

`HoldCause` shrinks to the causes that heal without a code change:
`storage_unavailable · capacity · quota · allocation · mount_unavailable ·
missing_tool · engine_unavailable · password_required`.

- **Persisted legacy causes are converted at boot**, before any generic
  handling: a stored `*Control:v1` with cause `io`, `identity_conflict` or
  `post_failure` becomes the terminal disposition §2.2 specifies for it
  (`io`/`identity` → `failed`, parked; `post_failure` → `failed/publish`,
  parked), with the event. Only a cause this build has never heard of
  (`Unrecognized`, written by a newer node) is treated as
  `storage_unavailable`: scheduled retry, deadline, never a dead end.
- A hold is a real status on the native API and SSE: `"status": "held"`. For
  one release the *persisted* status stays `Paused` with `control.lifecycle
  == "held"` as the discriminator (today's representation), and the native
  API/SSE project it as `held`; `JobStatus::Held` becomes the persisted
  variant only after every node runs that release, because a mixed-version
  cluster deserialising an unknown `snake_case` variant fails
  (`lib.rs:483`). The NZBGet-compat shim keeps projecting `paused`.
- carries `control` as today (`*Control:v1` unchanged on the wire: `cause`,
  `stage`, `retry_policy`, `next_retry_at_unix`, `blocked_since_unix`,
  `message`), plus `deadline_in_s` (seconds remaining, relative — hosts do
  not share a clock) and `deadline_unix` for display.
- has a **deadline** — `[post] hold_deadline_secs` (default `86400`;
  `password_hold_deadline_secs` default `604800`) — after which **runner** ends the job
  `failed/hold_expired` with the hold's message, bytes parked. Curator never
  authors `hold_expired` (§3.2): during a partition runner may recover the
  hold and complete the job, and a curator-side guess would then contradict
  the real outcome.
- **never locks its first cause**: a newer hold on a held job replaces the
  control (revision +1). Today the first cause wins (`owner.rs:6542-6546`).
- **keeps today's `manual_pause` rule**: an operator who pauses a held job
  suspends its automatic recovery until they resume it; the deadline clock
  keeps running and is shown.
- emits `job_control_changed` on every control write, as today.

### 2.4 Status and progress — `job_status_changed`, plus `tick`

A new event `job_status_changed {job, transfer, status, stage?, control?}`
fires on every `JobStatus` transition (including into/out of `held` and each
`PostStage`). `tick` stays as the 1 Hz progress snapshot. Curator stops
diffing ticks to discover transitions.

### 2.5 Publication and the manifest — kept, journaled, crash-safe; acknowledgement — removed

- Publication lands at `<completed>/<name>.job-<id>.runner-<gen>/`. The
  `job-<id>` is already in current names; it is what makes two attempts at
  the same release unable to collide, which is what lets curator drop the
  "runner owns failed bytes" hold.
- **Only selected files are published.** Same volume → each selected file is
  `renameat2(RENAME_NOREPLACE)`-moved (with the PR #247 fallback chain for
  mounts that refuse it) into the fresh generation directory; different
  volume → one streaming copy per file that produces the SHA-256 as it
  writes. **The manifest is written last** and is the commit marker. The
  processing directory, now holding only unselected siblings (par2 sets,
  samples, consumed archives), is runner's to dispose (§2.6). This keeps the
  property today's "always stage privately" branch (`relocation.rs:1140-1152`)
  exists for — unresolved siblings never enter `<completed>` — without the
  full copy and the 4–5 hash passes.
- **The move is journaled before it starts.** The relocation intent
  (generation, target directory, every selected file's identity — inode,
  size, journaled digest — and its destination name) is written to the
  artifact operation journal (`relocation.rs` `Operation`) **before the
  first rename**. On restart, reconciliation reads the journal and, for each
  intent without a manifest, either **finishes** the move (files already in
  the target are verified by identity and left; the remainder are moved;
  the manifest is written) or **unwinds** it (files moved back by identity;
  the target removed) — and only one of those, decided by whether the source
  is still complete. Disposal cannot claim either side while an intent is
  open. A crash after the first rename, after the last rename, after the
  manifest, and before the history/delivery writes each leave exactly one
  usable outcome after restart.
- The **manifest** is `.runner-manifest.json`: `{generation, job, transfer,
  created_at, cross_volume, files:[{path, bytes, sha256}], digest}`,
  mirrored in the history row params (`Delivery:Path/Generation/ManifestDigest`
  keep their names). `digest` is the **deployed `runner-manifest-v1`
  algorithm, unchanged** (runner `delivery.rs:161-175`, curator
  `delivery.go:1365-1384`): bare lowercase hex, no prefix. Curator verifies the manifest digest **once**, at
  import, if and only if `cross_volume` is true.
- **There is no acceptance.** `POST /api/v1/deliveries/{id}/accept`, the
  `accepted` state, `Delivery.status`, and curator's ack path are deleted
  (including their uses in `nzbd-cluster`). Runner retires its processing
  source once the manifest is durable and any salvage obligation has
  settled. For one release `accept` answers `200` as a no-op so an old
  curator's retry loop settles instead of retrying a `410` forever.
- `GET /api/v1/deliveries?after=<cursor>&include_accepted=true` stays as a
  **read-only reconciliation feed** of publications (immutable rows: job,
  transfer, path, generation, digest, created_at, kind `completed|salvage`,
  and the manifest body). Curator always passes `include_accepted=true`
  (the flag exists today, `delivery.rs:568`): a row an old curator
  acknowledged before placing its files is exactly the row a new curator
  must still see. Curator reads the feed on the poll to catch a `completed`
  or `publication_ready` it missed; it never drives import on its own when
  the event arrived.
- `GET /api/v1/history?since_seq=<n>` is the reconciliation feed for
  terminals, and its cursor is **`(incarnation, seq)`**: the response carries
  `incarnation` (a random id minted whenever the SQLite index is rebuilt from
  the portable JSONL log, `history.rs:315`, which reassigns rowids) and
  `retained_min_seq`. A consumer whose stored incarnation differs, or whose
  cursor is below `retained_min_seq`, has lost continuity and must
  **reconcile every outstanding transfer individually** through
  `GET /request-admissions/<token>` (which reports the job's outcome and is
  the one lookup that survives a history rebuild) before resuming the cursor.
  `limit=100` without a cursor is no longer how curator reads history.
- `GET /request-admissions/<token>` therefore has exactly two jobs: "did my
  `POST /jobs` with this token land, and as which job id" (during
  `submitting`), and "what happened to this transfer" after a continuity
  loss. It is **not** consulted to settle a terminal in normal operation; the
  event is.

### 2.6 Failure disposal and runner's own cleanup

After the terminal event, runner disposes the processing source per
`[post] failure_action` (`delete` default, `park`, `none`), overridden by the
job's `keep_on_failure` and by the park-always reasons (§2.2), **and only
once any salvage obligation for the job has settled** (`publication_ready`
emitted, or `salvage: none` recorded). The lifecycle override that forces
`Delete` regardless of config (`manager.rs:1054, 4404`) is removed: the
config means what it says.

Disposal is **owned by the artifact record and gated by the terminal
record, not the queue row**: the terminal record (§2.1) names the markers
(`*PP:done`, `*PP:failure-at`, disposition attempts) that are stamped on the
queue row so the normal sweep retires it, and the artifact inventory's
lifecycle tick runs the disposal only when the job's `job_terminals` row
exists with its salvage obligation settled — with its own bounded retry
(30 s/1 m/2 m/5 m, then hourly) and, in cluster mode, its own operation
lease — one node disposes. Disposal state is
visible on `GET /api/v1/artifacts` and the history row param `Failure:Files`
as today, and **nothing downstream waits on it**.

---

## 3. Curator — one reducer, two feeds, publications as the import unit

### 3.1 States

**Download row** (`downloads.state`, on-disk names in parentheses where they
differ):

```
 planned ─▶ grabbed ─▶ submitting ─▶ downloading ─▶ held ─▶ downloading
                           │              │           │
                           │              │           └─(runner: hold_expired)─▶ failed
                           │              ├─▶ completed (downloaded) ─▶ imported ─▶ notified
                           │              └─▶ failed ─▶ (blocklist if blamed) ─▶ next grab on a NEW row
                           └─(runner answered "no such token" for 10 min)─▶ failed{submit} [provisional]
```

**Publication** (new table `publications`, keyed `(download_id, generation)`,
`kind completed|salvage`):

```
 discovered ─▶ importing ─▶ imported | import_failed
                                │
                                ├─▶ notified  (outbox, §4)
                                └─▶ removed   (every file resolved and gone, directory gone)
 per file: pending ─▶ placed{library_path, sha256} | discarded{superseded|duplicate|rejected} | unresolved
```

The download row's `imported` means *its completed-kind publication has
every file resolved*; `notified`, cleanup and salvage imports belong to the
publication. `import_failed` on a publication is terminal for that
publication (a Resolve action; never a re-grab trigger) and the download row
shows it. `held`, `submitting`, `notified` are new row values; `downloaded`
stays on disk and means `completed`; `awaiting_import` is kept only for
`ManualApproval` clients and is the one non-terminal state whose exit is a
person — Activity shows its age and the approve action. `parked_reason`,
`superseded`, `runner_control`-as-gate, `request_token_idempotent`,
`delivery_expected` and the legacy review fences go away; live rows in those
shapes are migrated to `failed{reason: "parked: <old reason>", provisional}`
so the operator sees them once in Resolve instead of forever in Activity, and
a later runner fact can still settle them.

### 3.2 The reducer — facts, identity, "once", and who may author a terminal

`observe(dl, fact)` is the only function that changes a download row's state
from a client observation. Facts come from two feeds and are reduced the same
way.

**Identity.** A fact is admitted only if its `transfer` equals the row's
token; a mismatch is dropped and counted (`monarr_runner_fact_mismatch_total`)
— job ids are reused after a runner state wipe (`next_job_id` is
`max(history)`, `owner.rs:5431`), and the token is the only identity both
sides mint once.

**Once, durably.** The terminal is recorded as an immutable column
(`downloads.runner_outcome`, `runner_outcome_at`, `runner_outcome_seq`), set
by `UPDATE … WHERE id=? AND transfer=? AND runner_outcome IS NULL` — the row
count decides who won between SSE and the poll. **In the same transaction**
the work the terminal implies is written as a uniquely keyed row:
`completed` → a `publications` row (`UNIQUE(download_id, generation)`);
`failed` with a re-grab due → a `replacement_grabs` row
(`UNIQUE(download_id)`) that the grab worker executes idempotently. A crash
between "recorded failed" and "grabbed the next one" cannot lose the grab,
and a replayed `completed` cannot enqueue a second import.

**The history cursor belongs to the ordered history reader alone.** An SSE
event applies its terminal immediately (through the same once-guard) but
**never** moves `runner_history_cursors`; only the poll's
`/history?since_seq` reader advances it, after its page's terminals are
committed, to the last `seq` of that page (history ids have legitimate
gaps after pruning; "contiguous" is the SSE-never-moves-it rule, not a
claim about ids). So if SSE delivers
seq 42 while 41 was lost, 42 is applied now, the next poll still reads 41
(applies it) and 42 (a no-op by the once-guard), and the cursor moves to 42
only then.

**Who authors a terminal.** Runner-authored terminals (`job_pp_finished`,
`/history`, `/request-admissions` outcome) are final. Curator-authored ones
(`submit`, `lost`, and the migrated `parked:` rows) are **provisional**: any
later runner fact for the same transfer re-opens the row and is applied as
if the provisional terminal had not happened. Curator never authors
`hold_expired`: when a `held` row's `deadline_in_s` passes with no runner
fact, the row becomes `held (stale — runner has not reported since <t>)`,
reconciliation is forced on the next poll (`/jobs`, `/history`, then
`/request-admissions/<token>`), and it stays visible until runner answers or
`gone` applies.

| Fact | Source | Effect |
|---|---|---|
| `admitted{job}` | `POST /jobs` response, or `/request-admissions/<token>` while `submitting` | bind handle, → `downloading` |
| `status{status, stage}` | `job_status_changed`, `tick`, or poll `/jobs` | update stage/progress; `held` → `held` with control text and deadline |
| `terminal{outcome, …}` | `job_pp_finished`, or poll `/history?since_seq`, or `/request-admissions` after continuity loss | record once; `completed` → publication row (import now); `failed` → `handleFailure` (§3.3); `cancelled` → `failed{blameless, no re-grab}` |
| `publication_ready{publication}` | `job_publication_ready` or `/deliveries` kind `salvage` | publication row kind `salvage` (import now); download row stays `failed` |
| `gone` | poll `/jobs` has no such job **and** `/history` has no terminal **and** `/request-admissions/<token>` is not found, for `gone_grace` (10 min of *successful* polls — connection errors do not count) | `failed{reason: lost, blameless, no re-grab, provisional}` |
| `reopen{…}` | any runner fact (`/deliveries` row, history row, admission receipt, event) for a row whose terminal is provisional | revert the provisional terminal, apply the fact |

Ordering: a runner-authored `terminal` is accepted from any feed, once;
later `status` facts for a terminal row are ignored. A `terminal` ends a
`held` row unconditionally — there is no revision check between a hold and a
terminal. `acceptDownloadControl`'s refusal matrix
(`download_control.go:14-77`) is replaced by "store the newest control for
display"; the id-reuse case it guarded is covered by the token match above.

### 3.3 Failure → next grab, for every kind

`handleFailure(dl, terminal)` — all inside the terminal transaction except
the grab itself, which the `replacement_grabs` worker performs:

1. Record `fetched` for the budget.
2. Blocklist the **release identity** (indexer + GUID) **only if
   `blames_release`**. `io`, `publish`, `script`, `hold_expired`,
   `cancelled`, `lost` and `fetch{indexer_unavailable}` never blocklist; an
   `indexer_unavailable` failure re-queues the *same* want after the
   indexer's `retry_at` (the existing `RequestBudget` deferral) and writes no
   `replacement_grabs` row — grabbing an alternative from an indexer that is
   down is the storm, not the fix.
3. **Write the replacement grab** — unless the terminal is `cancelled` or
   `lost` (re-grabbing the release a person just removed is the loop the old
   park prevented; the want stays wanted and the planner's next pass handles
   it), or the row is `ManualAdditional` (one-shot: report and wait), or the
   row belongs to a `ReplacementOperation` (the operation owns the next grab
   and **advances with retained bytes** — it no longer waits on
   `payload_removed` or runner cleanup, `replacement_worker.go:430-447,
   960-968`). Otherwise the worker performs:
   - movie / book: `searchAndGrabBest` for the wantable;
   - episode: `searchAndGrabBest` for each affected episode wantable,
     **excluding season packs**;
   - episodes with no eligible single-episode release: those go to the
     season planner as `failed_acquisition` (the pack-fallback and "import
     only selected episodes" rules live there and are kept) — the planner is
     the fallback, not the path.
4. No custody wait. Runner's disposal of the failed bytes is runner's
   problem; the next attempt publishes under a different `job-<id>` and
   cannot collide.
5. Budget: `regrabSpend` counts **runner-reported fetched bytes only**
   (`fetched.bytes`, else `success_articles/total_articles × size`), and
   only for `blames_release` failures. Import failures, operator deletes,
   `cancelled`, `lost` and local faults cost nothing; the 20-failures ceiling
   per 12 h stays as the backstop against zero-cost loops.

### 3.4 Publication → import → notified → removed, per generation

On a `publications` row (`completed`, `salvage`, or a late one re-opened via
`reopen`): import enqueued immediately from its path. A directory without
`.runner-manifest.json` **or an equivalent v1 delivery record** (§6) is not
imported — logged and re-checked on the next poll, since runner may be
mid-publish. Manifest digest verified once if `cross_volume`. Import runs
file by file (`importDownloadFiles`), recording a **per-file outcome** on the
publication: `placed{library_path, sha256}`, `discarded{superseded|duplicate|
rejected}`, or `unresolved` (an obfuscated name with no episode mapping, a
sidecar with no parent). **Destination conflict policy**, the same for every
kind, and decided by **curator's own acquisition record, never by generation
ids or publication time** — generations are random 128-bit values
(`artifacts/mod.rs:418, :1211`) and an older attempt's salvage can finish
after its replacement imported: each wantable carries `current_attempt`
(the download curator most recently *chose* for it, by its grab/replacement/
upgrade decision). A file from the current attempt is placed under the
normal upgrade rules, replacing a library file from a non-current attempt
(whose publication file becomes `discarded{superseded}`). A file from a
non-current attempt whose wantable is already held by the current attempt is
`discarded{superseded}`. A file whose precedence cannot be established (no
acquisition record for the wantable, or two attempts both marked current by
a migration) is `unresolved` — retained, shown in Resolve, never deleted by
the sweep. A salvage arriving after its replacement imported, or a
replacement arriving after a salvage, both end with one library file and
one retained-or-discarded publication file, in either arrival order.

**In the same transaction** as the publication's last file outcome, the
outbox row for cinema is written (§4) and the download row advances.

**Cleanup is per file and evidence-bound.** Curator deletes a publication
file only when its recorded outcome is `placed` with a `sha256` matching the
manifest, or `discarded`; `unresolved` files are retained and shown in
Resolve; the directory is removed when it is empty. Every deletion
re-checks, in order: the publication generation (the manifest in the
directory still carries the same `generation`), the file identity (size and
manifest digest of the file about to be unlinked), and the **storage
identity** of the root (`StorageChangeReview` / live device identity — a
root whose identity changed is an unmounted mount point and nothing under it
is deleted). The hourly sweep applies the same rule to publications whose
files are all resolved and whose directory still exists. No "ask runner to
delete", no `payload_removed` reconciliation with runner.

**The historical cohort is frozen at cutover.** Every publication directory
that exists when the M2 build first starts (and every v1 delivery record,
accepted or not, that it migrates) is recorded with `cohort = historical`.
Historical publications are discovered, shown and **imported** through the
new path like any other, but **no automatic deletion touches them** — not
`cleanupAfterImport`, not the sweep — until the M7 report has been produced
and Paul has approved the batch that names them; approval sets
`cleanup_approved_at` on each publication, and only then does the normal
per-file rule apply. New publications are `cohort = live` and follow the
normal rule from the start.

The Delivery v1 import path (`delivery.go` discovery → lease → verify → ack →
import), `delivery_cleanup.go`'s receipt-bound deletion batches, the
migration-0040 legacy review cohort (`LegacyAutomaticImportReview` /
`LegacyCleanupReview` as *review* fences, **not** the storage-identity check
they wrap), and the per-row admission-receipt pacing are deleted.
`GET /deliveries` is read by the reconciler only to raise `terminal{completed}`,
`publication_ready` or `reopen` facts.

### 3.5 Visibility, deadlines, and the overdue predicate

| Row / publication state | Deadline (default) | On expiry |
|---|---|---|
| `submitting` | 10 min of runner answering "no such token" (unreachable does not count) | `failed{submit}` (provisional) |
| `downloading` with no `tick` for | 30 min (torrents: 24 h) | reconcile via `/jobs`; absent → `gone` grace |
| `held` | runner's `deadline_in_s` as received + 5 min | **`held (stale)`** + forced reconciliation; never a curator terminal |
| publication `discovered`/`importing` | 6 h | `import_failed{stalled}` on the publication; row shows it; a later runner fact cannot change it (it is curator's own work) |
| `awaiting_import` | none — operator-only exit, age shown | — |
| publication `imported` → `notified` | never fails; outbox owns it | — |

**The overdue predicate**, used by the health check `pipeline:overdue` and by
the plan's acceptance checks: `state NOT IN (terminals) AND deadline_at IS
NOT NULL AND deadline_at < now()`, where `deadline_at` is written on every
state entry; plus publications in `discovered`/`importing` older than 6 h.
Its count is `monarr_pipeline_overdue_rows` and must be 0 on a healthy
system; the health check names the rows.

Activity shows the state, the stage, the hold text, and the deadline; System
→ In flight is unchanged. Three durations are exported as Prometheus
histograms **and** persisted per transfer in `pipeline_timings` (so a
last-24 h p95 is a query, not a scrape artefact), shown on System →
Connections: `monarr_failure_to_next_grab_seconds` (runner terminal received
→ replacement grab submitted), `monarr_runner_done_to_import_seconds` (runner
terminal received → publication `imported`), and
`monarr_import_to_cinema_visible_seconds` (publication `imported` → cinema
reports the expected paths placed, §4.2). Imports still waiting are reported
as a separate count, never silently excluded from the percentile.
