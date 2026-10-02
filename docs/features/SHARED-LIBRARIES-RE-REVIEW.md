# Shared libraries: Opus S0 re-review of the revised contract

**Status:** historical re-review; corrections incorporated in the
[build contract](SHARED-LIBRARIES-IMPLEMENTATION.md#161-re-review-corrections--sl-18-through-sl-24).
The review below is retained as supplied. Its unpushed-branch statement was
already stale: draft PR #740 contains `eb9038755`. SL-08's chosen behavior is
labelled an engineering default, not an explicit Paul ruling. Current status
and dispositions belong to the build contract.

**Reviewed:** `docs/features/SHARED-LIBRARIES-IMPLEMENTATION.md` at local commit
`eb9038755` on `codex/shared-libraries-review`. The branch is based on `main`
@ `15e36f7f4`, holds only docs, and has not been pushed. **Code checked
against:** `15e36f7f4` · **Date:** 2026-10-02 · **Prior review:**
`SHARED-LIBRARIES-REVIEW.md` (SL-01 to SL-17)

**Verdict: close, but not yet ready to build.**

- SL-01 to SL-17: all dispositions accepted, with one confirmation needed
  (SL-08, below).
- Two new blockers. Both come from the revisions themselves, both are
  confined to the S3/S5 contract text, and both have simple corrections.
- Two major findings and three minor ones.

Fix SL-18 and SL-19, then mark the contract ready to build. The S2 topology
experiments stay explicitly pending, as §12 already says.

Branch hygiene:

- `test_docs_index.py`: 4 tests pass.
- `test_doc_status_audit.py` could not run on the session VM: it needs Python
  3.11's `tomllib` and the VM has 3.10. This is an environment limit, not a
  docs failure.
- No real host names, LAN addresses or `/Users` paths appear in the three
  docs.

## SL-01 to SL-17: confirmation

| ID | Confirmed? | Note |
|---|---|---|
| SL-01 | yes | Raw TCP Serve + loopback publish; Docker 28+; container egress qualified from inside the container. Correct, and the five S2 receipts are the right experiments. |
| SL-02 | yes | FQDN/IPv4 hints, Tailscale-resolver-only, admin override, pin before any secret. |
| SL-03 | partly | One family and one namespace is right. The schema fragment breaks; see **SL-19**. |
| SL-04 | partly | The full `StartResponse`/decision wire is right. "Change only URL-bearing fields" is not enough; see **SL-18**. |
| SL-05 | yes | Four counters, and only loss of effective authority stops playback. |
| SL-06 | yes | `remote_source` producer on a local-user session, plus an upstream side row. Login-binding is correctly called out as explicit S5 work. |
| SL-07 | partly | Signed keyset cursors are right. The revision fence has a liveness problem; see **SL-20**. |
| SL-08 | needs Paul | §3.1/§16 record this as "Ruling (a)". It was my recommendation, and my review left the decision to Paul. If Paul made the call, fine. If not, he should confirm it before S1. |
| SL-09 | yes | Deny matrix is now user-granular; key-expiry guidance included. |
| SL-10 | yes | Generated key, same-key renewal, credentialed manifest, explicit lost-pin path. |
| SL-11 | yes | — |
| SL-12 | yes | `prior_kbps = None` for shared starts; source-wide 8-slot cap. |
| SL-13 | yes | — |
| SL-14 | partly | The new FKs on `media_sessions` regress local deletes; see **SL-21**. |
| SL-15 | yes | — |
| SL-16 | yes | — |
| SL-17 | yes | Docs are isolated; land them via their own PR once confirmed. |

## New blockers

### SL-18 · §5.3, §7.2, §7.3, §8 · Clients build playback URLs themselves, so `/api/v1/shared/hls/…` breaks control, and file IDs reach local routes

The revision assumes B can change only "session identifiers and URL-bearing
fields". The clients do not get their URLs from those fields. They build
them from the session ID and the numeric file ID.

**Control path is hard-coded on both sides.**

- `PlaybackControlReporter.isNodeRelativePlaylist` (Apple
  `PlaybackControlReporter.swift:619`, Android `PlaybackControlReporter.kt:132`)
  accepts **only** `/api/v1/hls/{session}/index.m3u8|master.m3u8`. Android
  also accepts only `/api/v1/hls/{session}/control`.
- The server checks the same thing for prepared payloads
  (`playback_control.rs:2091`) and emits `/api/v1/hls/{session}/control`
  (`:137`).
- A relay at `/api/v1/shared/hls/{id}/master.m3u8` is therefore a protocol
  violation. The reporter refuses the action, and prepared replacement and
  held control are lost. This is the build-90 reporter trap class.

**File-keyed URLs are built from the numeric file ID.**

| Client | URLs built from the file ID |
|---|---|
| Apple `PlurxAPI.swift` | `files/{id}/decision` (399), `files/{id}/subs/{track}/overlay.json` (430), `files/{id}/subs/{track}/{path}` (492), `files/{id}/hls/sessions` (515) |
| Android `PlurxApi.kt` | `files/{id}/decision`, the overlay manifest and objects, `files/{id}/hls/sessions`, `hls/{session}/status`, `DELETE hls/{session}`, and `/api/v1/files/${fileId}/stream.mp4` (`Controller.kt:2602`) |
| Web | `stream.mp4` (`decode-tiers.js` ×4), `subs/{index}` (`audio-sync.js`), `chapters/{i}/thumb` (`watch-browser.js`), `/api/v1/hls/${sessionId}/control` (`playback-control.js`) |

**Failing sequence.** A viewer opens a shared title on B whose A file ID is
`412`. The client calls B's `GET /api/v1/files/412/decision` and later
`files/412/subs/2/overlay.json`.

- If B has a local file 412, B silently decides on and serves its own,
  unrelated file.
- If it doesn't, the call returns 404.

This is exactly the "numeric source ID into a local API" hazard §8 forbids,
and the contract gives the client no other URL to use.

There is also a pre-session gap. Decision and chapter thumbnails (on the web
detail page) happen before any relay session exists. §5.3 has no viewer-side
decision route, and §7.3 mounts chapter thumbnails only under an admitted
relay.

**Correction.**

1. **Serve B's relay under the ordinary `/api/v1/hls/{session}/…` routes.**
   §7.2 already makes it an ordinary local-user `media_sessions` row in B's
   one session-ID namespace. The HLS handlers dispatch on the producer kind
   (`remote_source` relays to A). The control reporter, `status`, `DELETE`,
   prepared payloads and `/control` then work unchanged on every client. Drop
   the `/api/v1/shared/hls` prefix entirely.
2. **Make file-keyed URLs come from one context string.**
   - Locally, the client keeps `/api/v1/files/{id}`.
   - For shared titles, the client builds every file URL from an opaque
     `file_base` that B returns in shared detail:
     `/api/v1/shared/imports/{i}/files/{opaque}`.
   - B serves `decision`, `hls/sessions`, `stream.mp4`, `direct`,
     `subs/{…}`, `overlay…` and `chapters/{n}/thumb` under that base. They
     are grant-checked and available before a session exists.
   - S7 carries the census above as its acceptance list: every call site is
     converted, and a source test fails on any new `files/${…}` built outside
     the context.

### SL-19 · §7.1 · The principal rebuild can't store a sharing row: STRICT PK columns are NOT NULL, and upserts target `(user_id, …)`

**Evidence.**

- `media_session_requests` has `PRIMARY KEY (user_id, request_id)` and
  `media_playback_pointers` has `PRIMARY KEY (user_id, playback_id)`, both
  `STRICT` (`hiqlite_sessions.rs:153,172`).
- In SQLite, a STRICT table's PRIMARY KEY columns are implicitly NOT NULL.
  This was verified on the VM (SQLite 3.37):
  `NOT NULL constraint failed: t.user_id`.
- The claim and upsert paths use `ON CONFLICT(user_id, request_id)`
  (`hiqlite_sessions.rs:1332, 1398`). An upsert can only target a partial
  unique index if it repeats that index's WHERE clause.

- The same claim enforces the per-user session cap with
  `(SELECT COUNT(*) FROM media_sessions WHERE user_id = $1) < $11`
  (`:1330`). With a NULL `user_id`, sharing rows would never be counted
  against any cap.

**Failing sequence.** On A, the first sharing start runs
`claim_media_session_request` with `user_id = NULL`. The insert fails on the
NOT NULL constraint. If the PK were relaxed instead, the existing
`ON CONFLICT` clauses would no longer detect replays, and a lost-response
retry would allocate a second encoder.

The §7.1 fragment and the "partial UNIQUE indexes" text address neither
problem.

**Correction.** Use one canonical owner column instead of nullable keys:

- `owner_key TEXT NOT NULL`, with the values `local:<user_id>` or
  `share:<grant_id>:<viewer_key>`, generated only by a typed constructor.
- Every composite key becomes `(owner_key, request_id|playback_id)`, and
  every `ON CONFLICT` targets it.
- Keep `principal_kind`, `user_id`, `share_grant_id` and `share_viewer_key`
  as checked projections, with a CHECK that ties `owner_key` to them.
- Local-only queries keep filtering `principal_kind = 'local'`.

This needs no partial indexes and no NULL semantics. The S3 census becomes
"every `user_id` predicate, conflict target and decoder".

## Major

### SL-20 · §6 · A whole-library revision fence makes paging fail during scans

§6 has `cursor_stale` restart the list whenever the library's revision
changes. The revision bumps on "every shared metadata/membership/order
change". In a library with an active scan, artwork repair or analysis jobs,
it can change faster than B can page through it. A large library (thousands
of items, 60 per page) then never finishes paging for a B viewer while A is
scanning.

**Correction.**

- **Narrow the fence.** Bump the revision only for membership changes and
  sort-key changes. Metadata-only writes don't affect order, and B re-reads
  them through the 30-second metadata TTL.
- **Resume, don't restart.** On a revision change, continue from the last
  `(sort_key, item_id)` under the new revision. B dedupes by reference.
- **State the guarantee honestly.** No duplicates; an item whose sort key
  moved behind the cursor during paging may be missed until the next open.
- **Test liveness.** S4 pages a large fixture library while a synthetic scan
  writes continuously.

### SL-21 · §7.1 · The rebuild fragment adds new foreign keys that block deletes

The fragment declares `user_id INTEGER REFERENCES users(id)` and
`share_grant_id TEXT REFERENCES sharing_exports(id)`.

- Today `media_sessions` has no FKs at all.
- Hiqlite enforces FKs: the vendored state machine sets
  `foreign_keys = ON`.
- User deletion is a bare `DELETE FROM users WHERE id = $1`
  (`hiqlite.rs:4474`) and does not clear session rows.

**Failing sequence.** An admin deletes a user who has any session row,
including the ended rows kept for terminal acks. The delete fails with an FK
violation, which is a local behaviour regression. The same happens when
deleting a revoked grant. Also, on B, `share_grant_id` would reference B's
own export table for no purpose.

**Correction.** Keep the rebuilt table FK-free, as today. Validate ownership
in the store, and tie retention cleanup to grant deletion explicitly.

## Medium and low

- **SL-22 (medium) · §10 · Upgrade procedure.** The coordinated full-cluster
  stop is stricter than the repo's existing schema-marker practice. Today an
  older binary refuses a newer cluster, failed nodes roll forward, and
  restore is the rollback path (OPERATIONS.md:629). Mandating a full stop
  forces a fleet-wide playback outage on every install, including those that
  never enable sharing. The actual safety invariant is narrower: **no
  sharing-principal row exists while any member runs a pre-principal
  binary.**
  - Enforce that at sharing admission, using a schema/protocol floor that
    every member reports.
  - Test that a running old binary's named-column reads, and its local
    inserts, which rely on `DEFAULT 'local'`, still work against the rebuilt
    table.
  - Keep the full stop only if that test fails.
- **SL-23 (low) · §2.2 · Clock skew on renewal.** A renewal with
  `notBefore = now` fails on a B whose clock is a few minutes behind.
  Backdate `notBefore` (for example by one hour) and keep the clock-skew test.
- **SL-24 (low) · §8.** Once SL-18 is in, §8's "start/progress adapter" text
  should name the `file_base` context and the census, so S6 and S7 don't each
  invent their own.

## Disposition table

| ID | Sev | Section | Disposition |
|---|---|---|---|
| SL-01–17 | — | §16 | accepted; SL-03/04/07/14 partly, via SL-18–21; SL-08 needs Paul's confirmation |
| SL-18 | Blocker | §5.3, §7.2–7.3, §8 | open: ordinary `/api/v1/hls` namespace on B; `file_base` context, pre-session file routes, client census |
| SL-19 | Blocker | §7.1 | open: `owner_key` canonical owner column in every key and conflict target |
| SL-20 | Major | §6 | open: narrow fence, resume rather than restart, liveness test |
| SL-21 | Major | §7.1 | open: no new FKs on session tables |
| SL-22 | Medium | §10 | open: admission floor instead of full-cluster stop, unless old-binary test fails |
| SL-23 | Low | §2.2 | open |
| SL-24 | Low | §8 | open |