# API — every endpoint plurxd serves, and what authorizes it

Companion to [ARCHITECTURE.md](ARCHITECTURE.md) (why there are two façades)
and [PLAYBACK.md](PLAYBACK.md) (how a file becomes a stream) — this is *what
you can call, which credential it takes, and what comes back*. Read
[SECURITY.md](SECURITY.md) for the trust model the credentials below
implement, and [INTEGRATION.md](INTEGRATION.md) for the Curator seam in
particular.

**There is no OpenAPI document.** The native API was designed to have one and
does not: `crates/plurxd/src/http/mod.rs` still says a description "will be
generated from these routes as they stabilize", and nothing generates it yet.
This file is the specification in the meantime, written by reading the routers
and the handlers on 2026-09-07. Where a plan document and the code disagreed,
the code won and the disagreement is recorded in §23.

One binary serves everything on one port (`:32400` by default). plurx has 210
routes across the four surfaces below. Every path here is absolute; the native
API is the only one under a version prefix, and §7-§18 state that prefix once
per section rather than repeating it in every row.

A test keeps that number and this inventory honest:
`tests/operations/test_api_doc_routes.py` parses the router and fails the
build when a route registered there has no entry here, when a path named here
is not a route and is not listed in §23 as deliberately absent, or when the
count above stops matching.

---

## 1. Surfaces — four things on one port

```
                         ┌────────────────────────────┐
   browsers, native      │        plurxd :32400       │
   clients, Curator ─────▶│                            │
                         │  /api/v1/…      native API │  §4–§18
   Kodi-family Plex ────▶│  /library/…     Plex Tier 1│  §20
   clients (X-Plex-Token)│  /identity, /:/timeline    │
                         │                            │
   the web app ─────────▶│  /, /assets/…, /icons/…    │  §22
   load balancer ───────▶│  /healthz /readyz /metrics │  §22
                         │                            │
   OTHER PLURXD NODES ──▶│  /_internal/… /internal/…  │  §21 — private
   (Ed25519 cluster auth)│                            │     control plane
                         └────────────────────────────┘
```

Four surfaces, four different rules:

- **Native `/api/v1`** — JSON, bearer or scoped-key credentials, WebSocket-free
  (clients poll). This is what the web app and the Apple and Android clients
  speak, and the only surface with any compatibility intent.
- **Plex-compat** — XML at Plex's own absolute paths, `X-Plex-Token` carrying
  a plurx token. Tier 1: the endpoint set the Kodi-family clients actually
  use. plex.tv is never contacted.
- **Web app and health** — unauthenticated static assets, plus the three
  endpoints a load balancer and a Prometheus scraper need.
- **Internal RPCs** — node-to-node only, signed with per-node Ed25519 cluster
  authority. Not public API, no compatibility promise, and account
  credentials are never accepted on it. §21 says this again, louder.

---

## 2. Authentication — two bearer kinds and five capabilities

### 2.1 One carrier chain, two credential kinds

Every bearer credential travels through one function, `token_from_parts`
(`crates/plurxd/src/http/extract.rs`), which checks three carriers in this
fixed order and takes the first hit:

1. `Authorization: Bearer <secret>` — the `Bearer ` prefix is stripped and the
   remainder trimmed.
2. `X-Api-Key: <secret>` — trimmed; an empty value is skipped as if absent.
   This is the header every \*arr application already sends, and refusing that
   convention would turn a valid key into a 401 that reads as "your key is
   wrong".
3. `?token=<secret>` in the query string, percent-decoded. `<img>`, `<video>`
   and `<track>` elements cannot set headers, so image and stream URLs carry
   the token inline.

There is no cookie path. Which *kind* of credential arrived is decided by a
prefix, not by the carrier:

| Kind | Shape | Minted by | Stored as |
|---|---|---|---|
| Login token | 64 hex chars (32 random bytes) | `POST /api/v1/auth/login`, `POST /api/v1/setup` | SHA-256 of the secret, in `tokens` |
| API key | `plx_` + 32 hex chars (16 random bytes) | `POST /api/v1/keys` | SHA-256 of the secret, in `api_keys` |

The `plx_` prefix says which table to look in *before* any lookup, so a key
and a token are never tried against each other's store and "not found" keeps
meaning something. Only the hash is ever persisted; the secret is returned
exactly once, at creation.

### 2.2 The four extractors

| Extractor | Accepts | Rejects with |
|---|---|---|
| `AuthUser` | any carrier resolving to a user row | 401 `authentication required` |
| `AdminUser` | `AuthUser` plus `user.is_admin` | 401, or 403 `admin privileges required` |
| `ScopedKey::require(scope)` | a `plx_` key, not disabled, carrying that exact scope | 401 for the wrong credential *kind*, 403 for a real key missing the scope |
| `CacheOnlyAdminUser` | a digest with a live admin proof in this process's cache | 401 only |

Admin-ness is re-read from the store on **every** request, so demoting a user
takes effect on their next request without revoking their token.

`ScopedKey` returns 401 rather than 403 for a login token because the
credential is the wrong *kind*, and 403 would suggest the right token could
work. The rule runs both ways, and both directions are load-bearing: a login
token must not open a key route, because "monarr can trigger scans" would then
be satisfiable with an admin token that can also read the TMDB and Trakt
secrets out of `GET /api/v1/settings`; and a key must not open a user route,
because a key has no user to attribute a watch state or a playback session to.

`CacheOnlyAdminUser` exists for exactly two recovery reads —
`GET /api/v1/cluster/status` and `GET /api/v1/cluster/support-bundle` — so
they still answer when the store cannot be reached. It never reads the store
and never falls back to it. A proof lives 5 minutes, capacity is 64 entries,
cache-only reads never renew it, and the whole cache fails closed while any
credential revocation is in flight. On a replicated backend it starts closed
and opens only once a committed roster proves every running member implements
peer revocation.

### 2.3 Scopes

`plurx_core::domain::scopes` is the complete, closed list — two entries:

```rust
pub const SCAN_TRIGGER: &str = "scan:trigger";
pub const STATUS_READ:  &str = "status:read";
```

Matching is exact string equality: no prefixes, no wildcards, and `disabled`
beats every scope. Two routes accept a key, and nothing else in the codebase
calls `ScopedKey::require`:

| Route | Scope |
|---|---|
| `POST /api/v1/scan` | `scan:trigger` |
| `GET /api/v1/scan/requests/{id}` | `status:read` |

`POST /api/v1/keys` rejects an unknown scope at creation rather than storing
it, because a typo'd scope would otherwise produce a key that looks correct in
the settings UI, authorizes nothing, and surfaces as a 403 inside another
application hours later. Every scope is a promise about what a stolen key can
do, which is why there are two of them and not twenty.

A key's `last_used_at` is refreshed at most once every 60 seconds, so a busy
integration does not append a Raft log entry per request. It never affects
authorization.

### 2.4 Capabilities — credentials that are not bearers

Five kinds of opaque identifier appear *in a path* and are themselves the
authorization. No auth extractor runs on those routes, and an `Authorization`
header there is ignored:

| Capability | Routes | Owned by |
|---|---|---|
| HLS session id (UUID v4) | `/api/v1/hls/{session}/…` | §9 |
| Offline media token (256-bit) | `/api/v1/offline/media/{token}/…` | §12 |
| Live-TV capability (`ltv1.…`) | `/api/v1/live-tv/sessions/{capability}/…` | §17 |
| Publication session | `/api/v1/publication/{session}/…` | §13 |
| Join-token digest | `/api/v1/cluster/join/…` | §19 |

The reason is always the same: the fetcher cannot set headers. Safari's native
HLS and an Apple TV during AirPlay fetch playlist URLs themselves; AVFoundation
and Media3 fetch child HLS resources autonomously; a closing browser tab sends
`DELETE` with `fetch(…, {keepalive: true})`, which cannot set headers either.
The ids are 122-bit random values minted for an authenticated user and reaped
on idle.

`GET /api/v1/cluster/artwork/{filename}` is a sixth non-bearer credential of a
different shape — a filename-bound cluster HMAC (§19.6).

Capability values never reach the access log: `safe_trace_target` replaces the
segment after `hls`, `publication`, `offline/media` and `live-tv/sessions`
with `[REDACTED]` and drops the query string entirely, so `?token=` and Plex
tokens do not survive into a span either.

### 2.5 Revocation and expiry

Login tokens **do not expire**. There is no expiry column and no sweeper;
`last_seen_at` is an activity signal refreshed at most once a minute. A token
stops working only when its row is deleted:

| Trigger | What is deleted |
|---|---|
| `POST /api/v1/auth/logout` | that one token digest |
| `PUT /api/v1/users/{id}` with a `password` | every token of that user |
| `DELETE /api/v1/users/{id}` | every token of that user, by cascade |
| `DELETE /api/v1/keys/{id}` | that API key |

Demotion (`is_admin: false`) deletes no tokens and does not need to, because
`AdminUser` re-reads the flag per request. Every revoking handler wraps its
store mutation in a cluster-wide cache revocation: a `Begin` phase fans out to
every committed peer, the SQL itself is made conditional on the revocation
lease still existing, and an `End` phase re-reads the roster so a member added
mid-operation is still invalidated. A lost lease is a 503 telling you to
retry; a fanout that cannot complete is
503 `admin_revocation_propagation_failed`.

Login is constant-time against unknown users: an absent username is verified
against a real Argon2id dummy hash, so unknown-user and wrong-password cost
the same and answer identically.

---

## 3. Errors — three envelopes, and the gates that answer first

### 3.1 The envelopes

**Legacy** — the common one, used by every `ApiError` variant below:

```json
{ "error": "human-readable message" }
```

| Variant | Status | Message |
|---|---|---|
| `BadRequest(msg)` | 400 | `msg` |
| `Unauthorized` | 401 | `authentication required` |
| `Forbidden` | 403 | `admin privileges required` |
| `NotFound(what)` | 404 | `{what} not found` |
| `Conflict(msg)` | 409 | `msg` |
| `UnsupportedMedia(msg)` | 415 | `msg` |
| `ServiceUnavailable(msg)` | 503 | `msg` |
| `Internal(msg)` | 500 | `internal server error` — `msg` is logged, never returned |

`StoreError` and `serde_json::Error` both map to `Internal`, so any storage or
serialization failure is a 500 with no detail.

**Unprocessable** — 422, body is a supplied JSON value verbatim with no
wrapper. It exists for one case, "path is not under any library root", which
lists the roots so a path-mapping mistake between two applications diagnoses
itself (§6.3).

**Typed** — the status is chosen by the call site:

```json
{ "code": "machine_readable_code", "message": "human-readable" }
```

A typed error may carry a flat detail map alongside; `code` and `message` are
inserted last so a detail field can never shadow the two keys clients dispatch
on.

There is **no central enum of typed codes** — each call site passes a
`&'static str`, and three more live as pre-serialized constants in middleware.
Roughly 75 distinct literals exist. The codes that matter are documented in
the section that emits them; the five below are emitted by middleware and are
therefore reachable on *any* route.

### 3.2 The gates that answer before any handler

Two middleware layers wrap the whole router, so they answer before routing,
before any auth extractor, and before every handler. They are the reason a
perfectly valid request can 503 on one node and succeed on its neighbour.

| Code | Status | Retry-After | Condition |
|---|---|---|---|
| `node_maintenance` | 503 | 1 | the node is in maintenance and the route is not on the maintenance allowlist |
| `learner_route_ineligible` | 503 | — | this node is a non-voting learner and the route is not on the learner matrix |
| `node_removal_fenced` | 503 | 1 | the node is draining or removed — or the serving role could not be read at all |
| `restart_drain_active` | 503 | 5 | the node is preparing for restart and refuses new *mutable media* work |
| `serving_fenced` | 503 | 1 | the node has lost quorum serving authority |

`learner_route_ineligible` carries no `Retry-After` on purpose: the refusal is
a role, not a transient.

**The maintenance allowlist** keeps enough surface alive to administer the
node and to drain capabilities already issued: `/healthz`, `/readyz`,
`/metrics`, `/`, the web assets, `GET`/`POST` of `/api/v1/auth/login` and
`/logout`, `GET /api/v1/me`, `/api/v1/activity`, `/api/v1/activity/detail`,
`/api/v1/system/logs`, the read-only cluster routes, the maintenance routes
themselves, and existing-media reads, control and close (HLS, publication,
offline media, live-TV sessions and the live-TV keepalive). Catalogue browsing
is deliberately *not* on it — browsing is what leads a client to pick this
ingress for a new stream.

**Fenced** is narrower still: only `/healthz` and `/metrics`. `/readyz` is not
exempt, so a removed node reports itself unready through this envelope rather
than through a readiness string.

`restart_drain_active` applies only to routes that *start* mutable media work
(HLS, offline, publication and live-TV creation, and the direct and remux
reads); `serving_fenced` applies to the `/files/`, `/hls/`, `/images/`,
`/publication/`, `/subs/`, `/offline/` and Plex `/library/parts|metadata/`
families. No `/api/v1/cluster/*` path matches either.

---

## 4. Server, accounts and sessions

| Method | Path | Auth | What it does |
|---|---|---|---|
| GET | `/api/v1/server` | public | Identity, version, and whether first-run setup is required |
| POST | `/api/v1/setup` | public, only while zero users exist | Creates the first admin and returns a token |
| POST | `/api/v1/auth/login` | public | Verifies credentials, mints a token |
| POST | `/api/v1/auth/logout` | bearer | Deletes this token's digest, cluster-wide |
| GET | `/api/v1/me` | bearer | The caller's own user record |
| GET | `/api/v1/users` | admin | Every user; never any password hash |
| POST | `/api/v1/users` | admin | Creates a user |
| PUT | `/api/v1/users/{id}` | admin | Sets password and/or admin flag |
| DELETE | `/api/v1/users/{id}` | admin | Deletes a user; tokens and watch state cascade |
| GET | `/api/v1/keys` | admin | Lists API keys; never the hash or the secret |
| POST | `/api/v1/keys` | admin | Creates a key, returning the secret **once** |
| DELETE | `/api/v1/keys/{id}` | admin | Revokes a key |

### 4.1 `GET /api/v1/server`

Credential-free in both directions, by design: a client probing an unknown
candidate must not attach a saved token, and the answer must carry nothing a
stranger should not read. The cluster's other ingress origins are therefore
*not* here — they live behind `GET /api/v1/cluster/ingress` (§19).

Fields: `name`, `version` (bare semver, which is what clients compare), `build`
(git description), `built_at`, `instance_id`, `node_id`,
`cluster_advertisement`, `uptime_seconds`, `setup_required`, `android_app`,
`playback_auto_abr`.

### 4.2 `POST /api/v1/setup`

`{"username", "password"}`. 409 `setup already completed` if any user exists —
checked before validation. 400 if the trimmed username is empty or the
password is under 8 characters. Success returns the same `{token, user}` shape
as login, so the client is signed in without a second round trip. The created
user is always an admin.

### 4.3 `POST /api/v1/auth/login` and `/auth/logout`

Login takes `{"username", "password", "device"?}` and returns
`{"token", "user"}`. Unknown user, wrong password, and a password changed
between the read and the token write are all the same 401.

Logout takes both the validated user and the raw token — the first so an
invalid token 401s rather than silently succeeding, the second so there is a
digest to delete. Returns `{"ok": true}`. Its two failure modes beyond 401 are
the cache-revocation ones from §2.5.

### 4.4 Users

`UserDto` is exactly `{id, username, is_admin, created_at}`; the password hash
is not in the DTO and cannot leak through these routes.

Two rules make a lockout impossible, and both are enforced inside the SQL
predicate rather than by a preceding count, so they cannot race: **the last
admin can be neither deleted nor demoted**, and **you cannot delete
yourself**. They surface as 409 `cannot remove admin from the last admin
account`, 409 `cannot delete the last admin`, and 400 `you cannot delete the
account you are signed in with`.

Passwords must be at least 8 characters. A duplicate username is 409. Setting
a password deletes every token of that user in the same transaction;
`is_admin: true` *plus* a password is executed as one statement rather than
two, so the demotion/reset pair stays inside a single peer bracket.

### 4.5 API keys

`KeyDto` is `{id, name, scopes, created_at, last_used_at, disabled}`. The
stored hash is deliberately excluded: listing keys is a routine, frequently
open settings screen, and the hash has no business on it.

`POST` takes `{name, scopes}` and refuses three things with a 400 that names
the fix: an empty name (`a key needs a name — it is how you will know which
one to revoke`), an empty scope list, and any scope outside §2.3. The response
is the only one that ever carries `key_secret`. It is not retrievable
afterwards, and losing it means issuing a new key — which is the correct cost
of never storing it.

---

## 5. Settings, diagnostics and activity

| Method | Path | Auth | What it does |
|---|---|---|---|
| GET | `/api/v1/settings` | admin | Full settings snapshot, from one store read |
| PUT | `/api/v1/settings` | admin | Partial update; returns the new full snapshot |
| GET | `/api/v1/system` | admin | Environment diagnostics and counters |
| GET | `/api/v1/system/logs` | admin | Tail of the in-memory log ring |
| GET | `/api/v1/system/playback-events` | admin | Node-local playback observations |
| GET | `/api/v1/system/library-shape` | admin | Codec and HDR census over the library |
| POST | `/api/v1/system/storage` | admin | Re-measures storage. Costs real I/O |
| POST | `/api/v1/system/search-index/rebuild` | admin | Rebuilds the derived search index on every voter |
| GET | `/api/v1/developer/readiness` | admin | Reports advisory observations for Playback, Cluster and Developer settings; never gates controls |
| POST | `/api/v1/client-log` | bearer | Files one client-side playback error into the server log |
| GET | `/api/v1/scan/status` | bearer | Per-library scan status |
| GET | `/api/v1/activity` | bearer | Flat list of what the server is doing |
| GET | `/api/v1/activity/detail` | bearer; admin sees more | The activity page |
| DELETE | `/api/v1/activity/sessions/{id}` | admin | Stops one transcode or VOD session |
| DELETE | `/api/v1/activity/offline/{id}` | admin | Cancels and deletes one visible offline package |
| DELETE | `/api/v1/activity/producer` | admin | Stops the pre-transcode producer after the current title |

### 5.1 Settings

`GET` builds its response from **one** snapshot read, not per-key reads: on
the clustered backend, reading each field independently turns one response
into dozens of leader barriers and can mix values from different commits.

The response returns configured secrets in cleartext — `tmdb_api_key`,
`omdb_api_key`, `monarr_api_key`, `trakt_client_id`, `trakt_client_secret` —
beside boolean `*_configured` companions. That is the concrete reason for the
key/token split in §2.3: an admin token handed to a neighbouring application
also hands over every secret on this route.

`PUT` is PATCH-shaped: **absent means unchanged**, and for credential fields
an **empty string clears**. `transcode_quality` is the one field that
distinguishes absent from explicit `null`; `null` restores the family-tuned
default. Every fallible field is validated and normalized *before* the first
write, because settings are persisted one at a time and a later bad field must
not leave an earlier policy change in force —
`dv_disk_keep_original = false` is the destructive case that forced the rule.

Live-TV settings are a separate transaction with their own generation
compare-and-swap, and mixing them into a request with any non-Live-TV field is
refused up front: a losing CAS must report 409 without an unrelated setting
having already committed.

Refusals worth knowing, each a 400 unless noted:

- `transcode_rate_mode must be bitrate or quality` — and whenever either
  rate-control field is sent, both are required, so a replicated update is one
  complete pair.
- `vod_working_set_bytes must be a number`, and a parsed **0** is refused
  rather than stored: 0 means "not configured" to the serving layer, so an
  operator who typed it would silently get a default.
- A `vod_blocked_get_cap` of 0 is refused because it would refuse every
  blocked GET — a setting that reads like "no limit" and behaves like "no
  playback".
- 409 when enabling `cluster_media_pool_enabled` before every committed voter
  publishes the protocol. Disabling always succeeds.

Job intervals (`probe_retry_mins`, `artwork_retry_mins`,
`transcode_cleanup_mins`, `cache_produce_mins`) take 0 to mean off and
otherwise have a floor of 15 minutes, because the scheduler ticks once a
minute and anything shorter would be a lie dressed as a setting.

### 5.2 `GET /api/v1/system`

Two of its sub-objects are shaped by a diagnostic argument rather than by
convenience.

`blocked_gets` reports five numbers together — `waiting`, `cap`, `admitted`,
`refused_session_busy`, `refused_pool_full` — because no subset answers the
operator's question. `waiting` without `cap` cannot distinguish a node at its
limit from one nowhere near it, and folding the two refusal classes together
cannot distinguish one client's seek storm (`session_busy`) from a genuinely
full node (`pool_full`) — and only the second argues for raising the cap.

`integration` (`notifications_received`, `scans_by_trigger`,
`last_notification_at`, `last_notification_source`, `last_correlation_id`)
exists because `scan_requests` only holds requests that got as far as a
library. A path-mapping mistake is rejected before a record exists, so a
server being called constantly and rejecting everything looks identical there
to one nobody is calling — which is the difference between "fix Curator's path
mapping" and "check Curator's URL and key".

`GET /api/v1/system/library-shape` is a separate route rather than a field
here because it is a table scan, and `/system` is polled by the settings page
every few seconds.

### 5.3 `POST /api/v1/system/storage`

`?sustained=<seconds>` adds continuous reading per mount on top of the quick
probe, clamped to `0..=120`, because a diagnostic that can be told to read for
an hour is a denial-of-service with a nice UI. The call is synchronous and
returns the freshly measured report; an immediate 200 carrying the old figures
would be worse than a slow one. It is a POST precisely so that reading the
last numbers through `GET /api/v1/system` is never the thing that goes and
takes new ones.

### 5.4 `GET /api/v1/system/logs`

`?level=` (default `trace`), `?limit=` (default 500, capped at 2000),
`?scope=cluster` to read the isolated membership and Raft ring instead of the
general one — two rings, so replication chatter cannot evict playback
diagnostics. Entries are `{ts_ms, level, target, message}`, oldest first.

### 5.5 `POST /api/v1/client-log`

Any signed-in user. It exists because when a browser refuses a stream — Safari
rejecting a codec, a file it will not progressive-play — nothing runs
server-side to fail, so the admin log stays empty and the failure is invisible
unless someone opens dev tools.

**It always returns 204**, including when the report is dropped: the client is
reporting, not asking, and an error response would only give it something new
to report about. Two-tier token buckets bound the pressure — 240 per user per
minute, 1000 node-wide, at most 4096 per-user buckets. The global bucket is
what stops a browser erroring in a loop from erasing the 2000-line log ring in
seconds, destroying exactly the history an operator opened the page to read.
Dropped reports are counted and the gap is reported on the next admitted line.
A poisoned limiter mutex fails *open*, because a lock bug must not silence
diagnostics.

`event` is one of `playback_failed`, `stream_rejected`, `hls_fatal`, `stall`,
`stall_recovery`. Of the two dozen optional fields, `decode_hw` is the one
that separates two failures every other field renders identically: a full
buffer with late frames because the GPU is doing the work and something
upstream hiccuped, versus a full buffer with late frames because a CPU is
software-decoding 4K. A `null` from a browser without
`navigator.mediaCapabilities` is honest; `false` is the finding.

### 5.6 Activity

`GET /api/v1/activity` returns a flat array; empty means idle. Each element is
`{kind, label, detail, percent}`, where `kind` is one of `scan`, `enrich`,
`stream`, `live_tv`, `produce`, `offline_prepare`, `offline_send`, `trakt`,
`cluster_degraded`. On a clustered node the local read and the bounded peer
round run concurrently, so a healthy peer adds no sequential wait; when peers
do not answer, a `cluster_degraded` entry is inserted first, naming up to
three missing nodes and counting the rest.

`GET /api/v1/activity/detail` is readable by any user — it is their household
server — but two parts of the payload are admin-gated and the three stop
actions are admin-only. `node_hostnames` is present for a clustered admin
**even when empty**, deliberately: the field's presence answers "may this
reader see machine names", and making an empty roster look identical to a
refused one would leave the gate untestable from the wire. `analysis` is
likewise admin-only, and degrades to `{"available": false, "enabled": <bool>}`
rather than failing the request.

`DELETE /api/v1/activity/producer` stops the producer **after the current
title**, not mid-encode, because the producer resumes from published segment
boundaries: a clean stop keeps the part already made and a kill throws it
away.

---

## 6. Libraries, scanning and browsing

| Method | Path | Auth | What it does |
|---|---|---|---|
| GET | `/api/v1/libraries` | bearer | Every library |
| POST | `/api/v1/libraries` | admin | Creates a library, then scans it |
| PUT | `/api/v1/libraries/{id}` | admin | Replaces name/kind/paths/anime, then scans |
| DELETE | `/api/v1/libraries/{id}` | admin | Deletes the library |
| PUT | `/api/v1/libraries/{id}/schedule` | admin | Sets automatic intervals without scanning |
| POST | `/api/v1/libraries/{id}/scan` | admin | Starts a background scan |
| POST | `/api/v1/libraries/{id}/refresh` | admin | Scans **and** forces metadata re-fetch |
| POST | `/api/v1/libraries/{id}/root-identity/reset` | admin | Forgets the root fingerprint so replaced storage can re-establish |
| GET | `/api/v1/libraries/{id}/items` | bearer | Paged, sorted, optionally genre-filtered grid |
| POST | `/api/v1/scan` | scoped key: `scan:trigger` | "Index exactly this path", for another application |
| GET | `/api/v1/scan/requests/{id}` | scoped key: `status:read` | Polls one targeted-scan request |
| GET | `/api/v1/items/{id}` | bearer | Item detail: item, ancestors, children, files, editions, reading |
| PATCH | `/api/v1/items/{id}` | admin | Hand-edits metadata — **home libraries only** |
| POST | `/api/v1/items/{id}/reanalyze` | admin | Re-runs ffprobe over every file on the item |
| POST | `/api/v1/items/{id}/refresh-artwork` | admin | Forces an artwork re-fetch for the item and its ancestors |
| GET | `/api/v1/items/{id}/photo` | bearer | Original photo bytes with range support, or the thumbnail |
| GET | `/api/v1/hubs` | bearer | Home rows: continue watching, next up, recently added |
| GET | `/api/v1/home/previews` | bearer | Per-library recent preview in one bounded read |
| GET | `/api/v1/search` | bearer | Prefix search across item titles |
| GET | `/api/v1/images/{filename}` | bearer | Cached artwork, materialized from a peer on miss |

### 6.1 Listing a library

`GET /api/v1/libraries/{id}/items` takes:

| Param | Default | Bounds |
|---|---|---|
| `sort` | `title` | `title` · `added` · `year` · `resolution` · `recorded`; an unrecognized value silently falls back to `title` |
| `offset` | `0` | negatives clamp to 0; no upper bound |
| `limit` | `60` | clamped to `1..=200` |
| `facts` | absent | only the literal `1` turns on the per-item `media` block |
| `genre` | absent | trimmed; matched `COLLATE NOCASE` against the stored genres array. An unknown genre is an empty page, not an error |

The response is `{items, total, offset, limit}`, where `offset` and `limit`
echo the *clamped* values and `total` is computed with the same genre clause
as the page, so paging never drifts into empty screens. Every per-row
decoration — watch state, resolution, `media` facts, child counts, and the
`{leaves, watched}` rollup on shows, seasons and folders — is a page-wide
batched query, never an N+1.

`GET /api/v1/search` takes `q` and the same `limit` clamp, and returns
`{results}` alone — no `total`, no paging. The query is split on every
non-alphanumeric character, lowercased, quoted per token, and the **last**
token gets a `*` prefix operator so typing is incremental. A query with no
alphanumeric characters returns an empty array with 200.

### 6.2 The item detail — the client contract

`GET /api/v1/items/{id}` returns `{item, ancestors, children, files,
editions, reading}`. `ancestors` is the parent chain outermost first, capped
at 16 hops as an anti-cycle backstop. `editions` lists other text and audio
editions sharing a *proven* `book_work_id` — title and author never populate
it. `reading` is non-null only for a book whose saved locator still matches
the current file revision.

Three per-file fields are the shared contract every first-party client renders
its detail screen from:

```jsonc
"audio_streams": [
  { "index": 1,          // index among AUDIO streams (ffmpeg a:{n}), not global
    "codec": "truehd", "channels": 8,
    "language": "eng", "title": "Atmos", "default": true }
],
"subtitle_streams": [
  { "index": 2, "codec": "subrip", "language": "eng", "title": "English",
    "default": true, "forced": false,
    "hearing_impaired": false }        // absent in older probe rows
],
"playback_defaults": {
  "audio":    { "selected_index": 1, "preferred_language": "eng",
                "preferred_language_status": "selected" },
  "subtitle": { "selected_index": 2, "preferred_language": "eng",
                "preferred_language_status": "selected" }
}
```

`preferred_language_status` is `selected` · `available` · `missing` ·
`unknown` · `no_tracks`. `available` is deliberately distinct from `selected`,
because the dual-audio anime rule can pick original-language audio when the
configured language is also present; `unknown` means an untagged track
prevents the "missing" claim; `no_tracks` lets a detail screen say "no
subtitles" instead of guessing. `playback_defaults` is computed from the
stored stream rows plus one settings snapshot — never from a playback decision
and never from a live probe — so an unmounted file still reports its tracks.

Also per file: `available` is one `stat` at request time, and `missing_path`
is added **only for admins**, and only when `available` is false.
`part_offset_ms` is the running offset within a multi-file audiobook, whose
files are sorted in natural numeric order so `Part 2` precedes `Part 10`.
`vod_index_status` is `indexed` · `partial` · `pending` · `refused` ·
`unsupported`, with `vod_index_refusal` carrying the builder's reason —
absent when nothing has been attempted, which is what `pending` honestly
means.

`tests/contracts/native-api.json` is the shared wire fixture compiled into
both native clients and gating the Rust lane. It omits every field whose
serializer drops it and includes a `future_server_field` in `server`, so
clients must tolerate both absent-optional and unknown-extra keys.

### 6.3 The targeted-scan seam

`POST /api/v1/scan` is how Curator says "index exactly this path". It is
deliberately **not** under `/libraries/{id}`: the caller knows a filesystem
path, not a plurx library id, and plurx resolving it is one less thing for two
applications to keep in sync.

```jsonc
{ "path": "/media/tv/Show/Season 01",   // required, must be ABSOLUTE
  "ids":  { "tmdb": 1234, "imdb": "tt0001234" },
  "hint": "movie" | "episode" | "season" | "book",
  "series": { "tmdb": 5678 },           // the SHOW's id, for episodes
  "book": { "title", "author", "medium", "work_id", "edition_id",
            "cover_url" },              // Books libraries only
  "correlation_id": "…", "source": "…" }
```

`path` must be absolute, because the caller and plurx may not share a working
directory. `hint` is advisory — the library's own kind decides how a file is
parsed, and the hint only picks which item an id applies to. `series` exists
because an episode's own tmdb id does not identify its series.
`correlation_id` is echoed and logged, so one grep reconstructs a transfer
across every application it passed through.

Library resolution canonicalizes the path and matches component-wise against
every root, **longest root wins** — so `/media` and `/media/kids` resolve to
the more specific library, and `/data` never matches `/database`. Nothing
matching is the one 422 in the API, and its body names every configured root:

```json
{ "error": "path is not under any library root",
  "path": "/downloads/x", "roots": ["/media/movies", "/media/tv"] }
```

| Status | Meaning |
|---|---|
| 200 `{"status":"scanned", …, "report", "items"}` | Ran synchronously to completion — a real filesystem walk and ffprobe work happened inline on the request |
| 202 `{"status":"queued", "request_id", …}` | The library was busy. **Queued, never dropped** — importing a season fires one request per episode within seconds, and dropping N−1 would leave the season half-indexed. Duplicates by path collapse |
| 400 | Relative path, or invalid `book` fields |
| 422 | Path under no library root |

The id hints ride *on* the queued request so the drained job applies them
later; an endpoint that applied them itself would drop them for every request
that arrived while a scan was running. Every call increments the integration
notification counter *before* path resolution, so a request rejected for a
path-mapping mistake still proves the caller reached plurx with a working key.

`GET /api/v1/scan/requests/{id}` returns the record verbatim. `status` is
`running` · `queued` · `done` · `failed`; `report` and `items` appear only at
a terminal state, `error` only on failure. Records live in a 256-entry
in-memory ring and 404 once evicted, so poll promptly.

### 6.4 Root identity, and why a scan refuses to prune

Each library records a fingerprint: the SHA-256 of its sorted, canonicalized
root paths. Before a scan deletes rows for files it did not see, it checks
that fingerprint. On a mismatch — or on an existing library with no recorded
identity whose scan saw no media — vanished-file cleanup is **skipped** and
the scan reports a note instead. That is what stops an unmounted NAS or a
container path mix-up from wiping a library.

`POST /api/v1/libraries/{id}/root-identity/reset` deletes that row so the next
scan can establish a new identity, for the case where an operator genuinely
replaced the storage. It returns `{"ok": true, "cleared": <bool>}`. It
authorizes no unbounded prune: establishment still requires a verified,
non-empty observation, and the independent prune bound still applies. The
fingerprint deliberately omits inode and device identity, because neither is
stable across voters for the same network export.

### 6.5 Side effects worth knowing

`POST /libraries` and `PUT /libraries/{id}` both trigger a background scan on
success. That is why the schedule has its own route: a settings page that
rescanned on every save would be a way to hammer a NAS by fiddling with a
dropdown. `PUT /libraries/{id}/schedule` takes
`{scan_interval_mins, refresh_interval_mins}`, where 0 turns a job off and any
positive value under **15** is a 400 — the loop ticks once a minute, and
scanning a real library every minute is a NAS denial-of-service with a
schedule attached.

`scan` and `refresh` both return `{"started": <bool>}`; `false` means the scan
lease was already held and this request did nothing. `refresh` is the
sledgehammer: it rescans *and* forces metadata re-enrichment for items already
matched, which is how season artwork gets backfilled onto older shows. Its
cost scales with library size and it re-hits the metadata providers.

`PATCH /items/{id}` refuses with 400 unless the library kind is `home`: movie
and show items are owned by their metadata agent, and a hand edit would be
overwritten by the next refresh. Its fields are doubly optional — absent means
"leave alone", `null` means "clear".

`POST /items/{id}/reanalyze` re-runs ffprobe over **every** file on the item,
not only the failed ones, synchronously, and returns
`{attempted, repaired, still_failing, gone, problems}`.

### 6.6 Images

`GET /api/v1/images/{filename}` takes a bare filename — no directories, no
traversal — and serves it `private, max-age=604800, immutable`. Seven days and
immutable means the URL itself must change when the bytes do, so every artwork
URL is built as `…?v={item.updated_at}`: the replicated item revision advances
on every artwork patch and makes an otherwise mutable filename a new cache
identity. The `?v=` value is a cache key only; the handler ignores it. There
are **no sizing parameters** — the stored bytes are what you get.

On a local miss the node fetches from a reachable peer voter and atomically
materializes the file: 8 concurrent materializations process-wide, 3 peers
raced, a 3-second total budget. Exhausting that budget is
503 `artwork_response_capacity`; a filename absent everywhere is 404.

`GET /api/v1/items/{id}/photo` serves the thumbnail for `?size=thumb` and the
original bytes with range support otherwise. When enrichment has not produced
a thumbnail yet it falls through to the original, because a real image beats a
broken one in a grid. Photos never touch the playback pipeline, and there is
no rotation or resize step: browsers honour EXIF orientation on `<img>`
natively.

---

## 7. Playback — the decision

Everything in §7–§11 is under `/api/v1`.

```
┌───────────────────────────────────────────────────────────────────────┐
│ CLIENT probes its own decoders, display and container support         │
└───────────────────────────────┬───────────────────────────────────────┘
                                │
     caps as query keys ────────┴──────── caps as a JSON document
     GET  /files/{id}/decision            POST /files/{id}/decision
     (vcodec, acodec, container, …)       {"caps":{"v":2,…}}  ≤ 64 KiB
                                │
                                ▼
┌───────────────────────────────────────────────────────────────────────┐
│ DecisionResponse                                                      │
│   method: direct_play | remux | transcode      reasons: [...]         │
│   delivery: {mode: direct|remux|transcode, ...}   ← execute THIS      │
│   play_url (legacy)  source  audio[]  subtitles[]  markers[]          │
│   ladder[]  prefer_segmented?  selection?  vod_indexed                │
└──────┬─────────────────────┬──────────────────────────┬───────────────┘
       │ direct              │ remux                    │ transcode
       ▼                     ▼                          ▼
┌─────────────┐   ┌──────────────────────┐   (no progressive form:
│ GET /files/ │   │ progressive:         │    always an HLS session)
│  {id}/direct│   │ GET /files/{id}/     │              │
│             │   │     stream.mp4       │              │
│ HTTP range: │   │ 200 only, ?start=    │              │
│ 200/206/416 │   │ seeks; no byte range │              │
└─────────────┘   └───────┬──────────────┘              │
       │                  │                             │
       │      GET /stream/{sid}/status ◀── ?stream=sid   │
       │                  │                             │
       │                  └── or, if the player needs   │
       │                      HLS transport (AVPlayer): │
       │                      POST sessions_url with    │
       │                      copy:true, aac:<bool> ────┤
       ▼                                                ▼
   plays as-is                          POST /files/{id}/hls/sessions  →  §9
```

| Method | Path | Auth | What it does |
|---|---|---|---|
| GET | `/files/{id}/decision` | bearer | Verdict and execution plan, from flat capability query keys |
| POST | `/files/{id}/decision` | bearer | The same, from a v2 capabilities document; body ≤ 64 KiB |
| PUT | `/files/{id}/audio-offset` | bearer | Deprecated. Validates and echoes `offset_ms` clamped to ±15 000; persists nothing |

Both decision handlers answer the same question and return the same body; the
`POST` validates the document, marks the caps as v2, and calls the `GET`
handler's logic directly. The only difference is where the capabilities live.
`force`, `audio` and `subtitle` stay on the query string in both forms,
because they are request-local choices rather than capabilities.

The **64 KiB body limit** is the same bound the reading-state route uses. A
capabilities document is a short list of codecs and a handful of learned
limits; axum's 2 MiB default is four orders of magnitude of headroom for an
authenticated caller to spend on a body the server has to walk.

### 7.1 Capabilities

Flat query keys, all optional; absent caps fall back to the named `profile`
(default `web-h264`), and CSV values are lowercase short names.

| Key | Meaning |
|---|---|
| `client`, `device` | Diagnostic labels only; never influence the decision |
| `profile` | Named fallback profile when no real caps are reported |
| `vcodec`, `acodec`, `container` | Decodable codecs and playable containers |
| `maxheight` | Global direct-play height ceiling |
| `vmaxheight` | Per-codec ceilings, e.g. `h264:1080,hevc:2160,av1:1080` |
| `hdr` | `1` when HDR may be shown directly — a *display* fact |
| `dv`, `dvprofile`, `dvhls` | Dolby Vision decode, probed profiles (authoritative over `dv`), and whether preserved DV needs the copy-video HLS envelope |
| `hdr10t` | `1` **only** when the client proved HEVC Main10 → PQ presentation |
| `capver`, `hdrtypes`, `dvdecoders`, `dvraw`, `dvstatus` | Android probe evidence; logged only |
| `force` | `auto` (default) · `original` · `transcode` |
| `audio`, `subtitle` | Request-local track indices; `subtitle=-1` is Off |

`hdr10t=0` and an absent `hdr10t` mean the same thing — *not proven* — and
both take the tone-mapped path. The server never reads a missing capability as
a capability the client lacks by choice.

The `POST` body is `{"caps": {…}}`, a `DeviceCaps` document with `v` **equal
to 2**; any other version is a 400 naming both versions rather than a
best-effort read. A client sending `v: 3` knows something the server does not,
and reading its fields as if they meant what v2's mean is how a device is
handed a stream it never claimed. Inside: `video[]` (per codec: `profiles`,
`max_height`, `max_bitrate_bps`, `present[]` of `sdr`/`pq`/`hlg`,
`dv_profiles`), `audio[]`, `containers[]`, `transports[]`, `dv_transport`,
`progressive_hevc_sample_entries`, `display: {hdr, dolby_vision, max_nits}`,
`learned_limits[]`, `max_height`.
An unrecognized `present` value deserializes to `Unknown` and matches nothing
rather than failing the whole document. Empty `containers` defaults to
`mp4,webm,mov`; empty `audio` to `aac,mp3`.

`progressive_hevc_sample_entries` is an additive, bounded progressive
packaging constraint. Missing or `null` preserves legacy behavior; `[]`
explicitly admits no progressive HEVC sample entry. A present list contains at
most four unique exact lowercase values from `hvc1`, `hev1`, `dvh1`, `dvhe`.
Semantic violations return typed **400 `invalid_capabilities`**. The field does
not grant HEVC decode, a profile, HDR presentation, or Dolby Vision support.

Both wire shapes translate into one `DeviceCaps` and then one `DeviceProfile`,
so a client upgrading from the query form to the document must get the same
verdict for the same hardware.

### 7.2 The response

| Field | Meaning |
|---|---|
| `method` | **The verdict**: `direct_play` · `remux` · `transcode` |
| `reasons` | Every dimension that failed. Empty ⇒ directly playable |
| `delivery` | **The execution plan — execute this, not `play_url`** |
| `play_url` | Legacy. For a transcode verdict it points at `stream.mp4`, the only progressive URL there is |
| `transcode_audio` | Re-encode audio to AAC because the source codec is outside the profile |
| `preserve_dolby_vision` / `convert_dolby_vision` | Keep DV signalling on a copy; rewrite Profile 7 RPUs to 8.1 in transit. The second is only ever true beside the first |
| `delivered_dynamic_range` | What the plan actually delivers: `dolby_vision` · `hdr10` · `hlg` · `sdr` |
| `delivered_dolby_vision_profile` | Omitted when the delivery carries no DV at all — which is not the same as unknown |
| `source` | Container, codec, profile, dimensions, bit depth, HDR format, DV profile, bitrate, duration |
| `audio[]`, `subtitles[]` | The selectable tracks |
| `selection` | Present only when `audio=` or `subtitle=` was sent |
| `markers[]` | Skippable regions |
| `ladder[]` | `{height, total_kbps, peak_kbps}`, top rung first, filtered to what the source can feed |
| `prior_kbps` | Node-local sustained-throughput prior for this client/network tuple; absent while the opt-in feature is off or the tuple is cold |
| `prefer_segmented` | A hint, not an instruction |
| `vod_indexed` | This node has a fragment index matching the file and the copy-video identity this decision selected |
| `audio_offset_ms` | **Always zero.** Retained for older clients |

`delivery` is tagged by `mode`:

- `{"mode":"direct","url":"/api/v1/files/{id}/direct"}`
- `{"mode":"remux", "url":"…/stream.mp4[?audio=N]",
  "sessions_url":"…/hls/sessions", "aac":<bool>,
  "preserve_dolby_vision":<bool>, "requires_hls":true?}` — the same bytes in
  two envelopes. A
  player needing HLS transport POSTs `sessions_url` with `copy: true` and
  this `aac` instead of fetching `url`. `requires_hls` is omitted when false;
  when true the progressive URL is not an executable alternative for this
  caps snapshot, including on cold-index fallback.
- `{"mode":"transcode","sessions_url":"…"}` — POST it *omitting* `height`:
  Auto is the server's choice, because the rung depends on which encoder wins
  and only the create response knows that.

Only an explicit `subtitle=` may change the delivery verdict; an audio-only
request still echoes the effective policy subtitle but does not burn it.
`selection.subtitle_requires_burn_in` marks a bitmap track with no enabled
overlay route, and `selection.subtitle_burn_in_blocked_by_hdr` marks the case
where the HDR guard refused the burn rather than silently replacing HDR or DV
with SDR.

On a subtitle track, `text` and `native` are different claims: `text` means a
WebVTT sidecar can be extracted, and is true for every non-bitmap codec;
`native` means the track can become an HLS WebVTT rendition, and is true only
for `subrip`, `srt`, `webvtt`, `vtt`. An ASS/SSA or `mov_text` track is
`text: true, native: false` — the sidecar works, the rendition does not, and
creating a session against it is refused with *"the selected subtitle requires
burn-in"*.

`prefer_segmented` appears only on a remux verdict for a file with a probed
bitrate, and its value is a sentence, e.g. *"38 Mb/s remux — progressive fMP4
buffers about 2.2 s; HLS allows deeper read-ahead"*. It is a hint because the
server knows the bitrate and the storage read rate, but only the browser knows
whether its MSE implementation will accept the codec — so the client verifies
before acting and falls back to the progressive path.

Errors: 404 for a missing row; **409** when the row exists but the path is not
on disk (*"this media file is missing on the server — its library path may be
unmounted, moved, or renamed"*); 400 for an unknown track index or an
unrecognized capabilities-document version. Typed **409
`unsupported_hevc_delivery`** means the actual progressive copy output was not
admitted and the document did not claim HLS; session create performs the same
check before durable session admission.

### 7.3 How to read the decision

`method: "direct_play"` with an empty `reasons` and `delivery.mode: "direct"`
is the goal state: nothing is spent. `method: "remux"` whose `reasons` name
only the container and the audio codec is the second-best answer — the video
is copied untouched, so `delivered_dynamic_range` will equal the source's
grade unless a Dolby Vision strip is in play. In both cases
`delivered_dynamic_range` should match `source.hdr`, with `null` reading as
`sdr`.

`method: "transcode"` is expensive by definition, so read `reasons` first: it
names every dimension that failed, and the entry mentioning HDR usually means
the client sent `hdr10t=0` or omitted it. A `delivered_dynamic_range` of
`sdr` against a `source.hdr` of `dolby_vision` or `hdr10` is the downgrade a
viewer will see as washed-out colour. An absent
`delivered_dolby_vision_profile` on a DV source is *no answer*, not *not
Dolby Vision*; `delivered_dynamic_range` beside it is the field that answers
that.

If the verdict looks wrong, the capabilities are the first suspect: re-run the
same request as the `POST` form with an explicit document and compare, since
both shapes route through one translation and must agree.
`vod_indexed: false` means this node lacks the matching local copy index.
Session creation can hydrate an exact shared artifact; otherwise it retains
the rolling first-play fallback while enabled shared preparation queues the
missing source/recipe work. It does not wait for a full-file index pass.

---

## 8. Playback — direct play and progressive remux

| Method | Path | Auth | What it does |
|---|---|---|---|
| GET | `/files/{id}/direct` | bearer | Original file bytes, full HTTP range support |
| GET | `/files/{id}/download` | bearer or query token | Original file attachment, full HTTP range support; does not register playback |
| GET | `/files/{id}/stream.mp4` | bearer | Progressive fragmented-MP4 remux from a live ffmpeg pipe |
| GET | `/stream/{id}/status` | bearer, owner-checked | Progress of one progressive remux |

These two differ in a way that is not a byte-serving detail — it is why
seeking works differently on each.

### 8.1 `GET /files/{id}/direct` — full range support

| Condition | Status |
|---|---|
| No `Range` (or a `HEAD`) | **200** with `Accept-Ranges: bytes` and the full length |
| `Range` present **and** `If-Range` present | **200**, range ignored — these raw-file routes publish no strong validator, so an `If-Range` precondition cannot authorize a partial response |
| Unknown range unit (`items=0-1`) | **200** — an origin must ignore an unknown unit |
| Suffix `bytes=-N`, `N>0`, on a zero-length file | **200** — a nonzero suffix is satisfiable even for an empty file, and the alternative is inventing `-1` |
| Satisfiable range | **206** with `Content-Range` |
| Malformed spec, non-UTF-8 header, two `Range` lines, more than 16 members, or first-position > last | **416** with `Content-Range: bytes */{len}` |
| File missing on disk | **404**, and the availability cache is invalidated |

A multi-member list is fully validated before its first usable member is
chosen, so a list containing one bad member fails rather than silently serving
the good one. Positions are parsed with saturation, and the original decimal
digit strings are compared before saturation — two huge numbers can both
saturate to the same value while still being ordered.

`?stream=<id>` is optional and groups the *range storm*: a seeking browser
makes dozens of 206 requests for one film, and without an id they are grouped
by file instead, which merges two simultaneous plays of one title by one
person into one activity row. Merging is the safe error; the other direction
puts phantom viewers on the activity page. Repetition is also what keeps a
direct play listed at all — it has no session to end, and a closed tab
announces nothing.

### 8.2 `GET /files/{id}/stream.mp4` — no range support at all

The response is always **200**, `Content-Type: video/mp4`, with a live ffmpeg
pipe as the body. It reads no `Range`, sets no `Accept-Ranges`, sets no
`Content-Length`, and can never return 206 or 416. **Seeking is done by
re-requesting with a different `?start=`**, which becomes an input-side (fast)
seek; copied video starts at the preceding keyframe so the matching audio
preroll is retained.

Query: `start` (seconds), `audio`, `stream`, `audio_offset_ms` (clamped to
±15 000, forced to 0 for a file with no audio), plus the same flat capability
keys as `/decision` — so the remux copies the audio when the browser can play
it rather than re-encoding to AAC needlessly. It does **not** accept the v2
document.

One response header carries what a JSON envelope would have:
**`x-plurx-media-origin-ms`**, the source-timeline timestamp represented by
local zero. It is a header because the body is the MP4 byte stream. It is
probed in parallel with the ffmpeg start under a 1-second budget; on timeout
the requested `start` is used and a warning is logged.

The remux is `-c:v copy` with
`-movflags frag_keyframe+empty_moov+default_base_moof+delay_moov` and
`-avoid_negative_ts make_zero`. HEVC copies are tagged `hvc1`, because an
`hev1`-tagged MKV copy plays audio-only or black in Safari.

### 8.3 `GET /stream/{id}/status`

A progressive stream is not a session, so it answers from its own registry —
same question as the HLS `status` route, different bookkeeping. `{id}` is the
`?stream=` value the client sent. Registration is unconditional (a stream with
no client id is registered under a server-minted `srv-<uuid>` so the activity
page can see it), but only a client-known id can be looked up here. **This is
not a capability**: the lookup filters on `user_id`, so the check is that the
asker owns the stream, and 404 covers unknown, finished and someone else's
alike.

| Field | Meaning |
|---|---|
| `speed` | Cumulative encode rate as a multiple of realtime |
| `recent_speed` | The rate over the last few seconds — **the one that says whether the server is keeping up now.** The cumulative figure hides a slowdown behind a fast start |
| `out_time_ms` | Content produced, in ms from this stream's start offset |
| `delivered_bytes` | Total bytes handed to this client |
| `delivered_bps` | Recent delivery rate in **bits** per second, or `null` before a window closes. Bits, because that is the unit a viewer's link is sold in |
| `delivered_idle_ms` | Age of that rate. A client with a full buffer stops fetching, which is health, not a slow link — the rate keeps its last real value and this says how old it is, so a reader can tell a measurement from a memory |
| `readrate` | Configured input pace as a multiple of realtime, so a client can say "at its limit" rather than implying the machine is the constraint. Default 4.0, admin-settable, `0` disables pacing; the first 30 s is delivered flat out so seeks stay instant |

Entries disappear when the response body is dropped, which is the same moment
the ffmpeg is taken down.

---

## 9. Playback — HLS sessions

```
  POST /files/{id}/hls/sessions      (GET /files/{id}/hls/start is the
    body ≤ 64 KiB, keyed by           deprecated query-string bridge
    playback_id → reaps THIS          onto the same handler)
    playback_id's predecessor,
    spawns an encoder
                 │ StartResponse: session_id, playlist_url,
                 │ media_origin_ms, height, encoder, vod,
                 ▼ ladder[], control?, plan_notes[]
┌────────────────────────────────────────────────────────────────────┐
│ SESSION — from here the session id IS the credential.              │
│                                                                    │
│  GET /hls/{session}/master.m3u8      (native-subtitle multivariant)│
│        ├─▶ GET /hls/{session}/video.m3u8        (video rendition)  │
│        └─▶ GET /hls/{session}/subs/{index}/index.m3u8              │
│                  └─▶ GET /hls/{session}/subs/{index}/segNNN.vtt    │
│  GET /hls/{session}/index.m3u8       (plain media playlist)        │
│        └─▶ GET /hls/{session}/{segment}                            │
│              init.mp4 | segNNNNN.ts | segNNNNN.m4s                 │
│              ETag + Range/If-Range → 200 or 206                    │
│                                                                    │
│  GET  /hls/{session}/status    ── how production is doing          │
│  POST /hls/{session}/control   ── the control exchange, ≤ 16 KiB,  │
│            every ~5000 ms; renews the lease, returns an action     │
│            none | hold | terminal | retry_resource | prepare       │
│                                       │                            │
│                          prepare ─────┘ stages a SUCCESSOR session;│
│                          the client opens its playlist_url, then   │
│                          acknowledges Committed → old one released │
└───────────────────────────────┬────────────────────────────────────┘
                                │ tab closes / player done
                                ▼
                  DELETE /hls/{session}   → 204 No Content
```

| Method | Path | Auth | What it does |
|---|---|---|---|
| POST | `/files/{id}/hls/sessions` | bearer | Creates a session; body ≤ 64 KiB |
| GET | `/files/{id}/hls/start` | bearer | Deprecated bridge over the same handler |
| GET | `/hls/{session}/master.m3u8` | **capability** | Multivariant playlist with subtitle renditions |
| GET | `/hls/{session}/index.m3u8` | **capability** | Media playlist (the historical shape) |
| GET | `/hls/{session}/video.m3u8` | **capability** | The video rendition the master references |
| GET | `/hls/{session}/subs/{index}/index.m3u8` | **capability** | One native WebVTT rendition's playlist |
| GET | `/hls/{session}/subs/{index}/{segment}` | **capability** | One `segNNN.vtt` slice |
| GET | `/hls/{session}/{segment}` | **capability** | One media segment, or `init.mp4` |
| GET | `/hls/{session}/status` | **capability** | Production and delivery telemetry |
| POST | `/hls/{session}/control` | **capability** | One bounded control exchange; body ≤ 16 KiB |
| DELETE | `/hls/{session}` | **capability** | Releases the session |

### 9.1 Create — and why it is a POST

A body rather than a query string, and a POST rather than a GET, because this
call spawns a process and kills its predecessor. A GET that does that is a
trap: GET is idempotent by definition, so anything entitled to replay one — a
retry, a prefetch, an intermediary — could spawn a second encoder and orphan
the first.

**Supersession is keyed on `(user, playback_id)`**, not on the account and not
on the file. One player instance seeking, changing track or changing quality
replaces its own stream; two devices on one account no longer kill each
other's. Cluster replacements skip the break-before-make sweep entirely and
keep the predecessor serving until the durable activation succeeds.

`playback_id` is the only required field (1–128 bytes, no CR, LF or NUL). The
rest, in the groups that matter:

- **Idempotency and recovery** — `request_id` (one attempt),
  `control_sequence` (the exchange this restart was decided from, so a restart
  the viewer has already scrolled past can be skipped), `previous_session_id`
  and `reopen_reason` (both required for a stall reopen, which also requires
  `request_id`).
- **Output** — `height` (ignored when `copy`; **omitted means Auto, and Auto
  is the server's decision**), `quality_auto` (whether the *viewer* chose
  Auto — not the same question as whether this body carried a height, since a
  subtitle burn and Quality=Original both send one), `copy`, `aac`,
  `preserve_dolby_vision`, `hdr10`.
- **Tracks** — `audio`, `subtitle` (an initially selected native rendition,
  HLS metadata only), `native_subtitles`, `subtitle_burn`,
  `subtitle_burn_sdr`, `audio_offset_ms` (±15 000, never written back to the
  file).
- **Position** — `start` in seconds, negatives clamped to 0.
- **Evidence** — `caps` (the same v2 document), `overrides`, `presentation`
  (only `"vod"`), `block_budget_secs`, `intent` (the viewer's ask as an
  *orderable* value, recorded durably before anything is built).

`convert_dolby_vision` is deliberately **not** a wire field: a client has no
way to be right about it, so it is derived from caps and the node's own
probes. More generally, when `caps` is present the server re-derives the plan
and `preserve_dolby_vision` and `hdr10` become *assertions*: a disagreement is
logged, counted, and the server's plan wins — it is never a refusal. When
`caps` is absent the client's echo is trusted, as it always was. Five
process-lifetime counters track which path each create took
(`legacy_trusted`, `unusable_caps`, `rederived`, `mismatched`, `overridden`),
and they are reported by `GET /api/v1/system`.

The response carries `session_id`, `playlist_url`, `duration_ms`,
`start_seconds`, `media_origin_ms`, `height`, `encoder`, `vod`, `ladder`,
`prior_kbps`, `delivered_dynamic_range`, `delivered_dolby_vision_profile`,
`control`, `plan_notes`.

- `media_origin_ms` is the source-timeline timestamp represented by
  player-local zero. A copy session can start on the keyframe *before*
  `start_seconds`; an accurate transcode starts at the request.
- `vod` means the whole stream already exists on disk (a pre-transcode cache
  hit), so the player treats it like direct play and does not arm the stall
  watchdog's restart.
- `delivered_dynamic_range` here **overrides** the decision's answer the moment
  the session attaches — the decision described a plan, this describes bytes.
- `playlist_url` is the master when `native_subtitles` was set. The master
  gets its own path rather than a query flag because AVPlayer caches by URL
  and would otherwise conflate `index.m3u8?native=1` with the child
  `index.m3u8`.
- `control` is `{protocol: "plurx-playback-control-v1", url, generation,
  control_epoch, next_exchange_ms: 5000, lease_timeout_ms}` where the timeout
  is 60 000 (rolling) or 300 000 (VOD).
- `plan_notes` says why this session's plan is not the one the client asked
  for — a named override, or a `plan_mismatch:` line naming the field, the
  client's value and the server's. Empty and omitted on every agreeing create.

| Status | Code | When |
|---|---|---|
| 400 | — | `playback_id` outside 1–128 safe characters |
| 400 | — | `unknown native subtitle track` / `the selected subtitle requires burn-in` |
| 409 | — | `request_id` was already used for a different session intent |
| 410 | `live_presentation_removed` | `presentation` is anything but `"vod"` |
| 410 | `media_session_ended` | the idempotent session was already released |
| 422 | `hdr_subtitle_burn_refused` | a burn on a known-HDR source without `subtitle_burn_sdr: true` |
| 503 | `media_session_starting` | an identical request is still starting |
| 503 | `media_session_handoff_pending` | the predecessor owner has not finished handoff |
| 503 | `media_session_superseded` / `playback_target_superseded` | the client has since settled on a later destination |
| 503 | — | too many session starts already active for this user |

**The deprecated bridge.** `GET /files/{id}/hls/start` accepts `height`,
`start`, `audio`, `copy=1`, `aac=1`, builds a create body and calls the same
handler. It supplies `playback_id` as `legacy:{username}:{file_id}` — so every
legacy start by one user on one file supersedes the last — and hard-codes no
capabilities document, no burn, and `preserve_dolby_vision: false`. With no
capabilities it lands on the trust path with every other pre-caps client. New
clients should not use it.

### 9.2 Playlists and segments

All playlists are `application/vnd.apple.mpegurl` with `Cache-Control:
no-store`, and relay to the owning cluster node when this node is not the
owner.

`master.m3u8` takes `?subtitle=` (initially selected rendition) and
`?diagnostic=` (`video-only`, `video-only-codecs`, `video-only-range`,
`video-only-hdr`). The diagnostic modes only *remove* declarations and never
expose another resource. Ordinary masters declare HLS version 7; version 10 is
used only when `SUPPLEMENTAL-CODECS` is declared, because advertising that
from a version-7 master makes AVPlayer reject an otherwise valid Profile
8.1/8.4 rendition.

Before a master is published, plurx reads the exact initialization object for
that session and validates its fMP4/HEVC codec description. The inspection is
part of the existing five-second response-publication budget and is fenced to
the same owner as the playlist response. It never falls back to scanner-guessed
codec metadata when the published init cannot support that claim.

`GET /hls/{session}/{segment}` serves `init.mp4`, `segNNNNN.ts` or
`segNNNNN.m4s` and nothing else. Content types follow Apple's HLS authoring
profile — `.ts` → `video/mp2t`, `.m4s` → `video/iso.segment` — because
labelling an `.m4s` as `video/mp4` still decodes but can make AVPlayer's
multivariant validator reject the rendition before opening the decoder.

Segments carry `ETag`, `Accept-Ranges: bytes` and `Cache-Control: private,
max-age=3600, immutable`, and honour `Range`. `If-Range` is stricter than
`If-None-Match`: only an exact **strong** entity-tag authorizes a partial
representation, and weak tags, dates and malformed values all fall back to a
complete 200. `init.mp4` inspection is bounded at 1 MiB. A larger, short, or
malformed init is an invalid publication rather than a truncated response or a
best-effort success.

| Status | Code | Meaning |
|---|---|---|
| 404 | — | `segment not found` |
| 502 | `hls_init_invalid` | Published init bytes are malformed, incomplete, oversized, or missing a required codec record; terminal for this session |
| 502 | `hls_init_unsupported` | The init is valid fMP4 but its codec/sample-entry layout is unsupported; terminal for this session |
| 503 | `startup_timeout` | The session's initialization media is still pending inside the bounded playlist publication window |
| 503 | `init_inspection_unavailable` | Storage or inspection capacity could not provide a trustworthy init reading; a bounded retry is reasonable |
| 503 | `segment_pending` | still being produced; retry shortly |
| 503 | `response_snapshot_capacity` / `response_publication_timeout` | node-side capacity |
| 503 | `media_owner_transition` | a successor may still arrive |
| 410 | `media_owner_lost` | no successor is coming |
| 410 | `media_session_ended` | gone; waiting will not help |

The two owner errors carry `{film_position_ms, film_frontier_ms,
reopen_required, continuous}` so a client that stops waiting knows where to
reopen. `continuous` is always `false` — a recovered session is renumbered
across an `#EXT-X-DISCONTINUITY` — and `media_owner_lost` is a 410 rather than
another 503 so that a client understanding no code at all still reads "gone"
and stops waiting.

### 9.3 `GET /hls/{session}/status`

Registered outside the `{segment}` catch-all: a static path segment wins over
a parameter regardless of registration order, and `status` is not a valid
segment name anyway.

The body is an **untagged** two-arm enum, so existing live clients see
byte-for-byte the object they already consume. VOD is the primary
presentation, and its arm carries `producer_state`, `producer_hold`,
`producer_decision`, `published_end_ms`, `ready_ahead_end_ms`,
`fetched_end_ms`, `ahead_seconds`, `materialized_segments`/`_bytes`,
`planned_segments`/`_bytes`, `working_set_bytes` and its budget, `admitted`,
`delivered_bytes`, `delivered_bps`, `suspended`, `final`.

`published_end_ms` and `ready_ahead_end_ms` answer different questions.
`published_end_ms` is how much of the title exists, measured from segment 0;
`ready_ahead_end_ms` is the end of the contiguous run measured from the
segment *this client* was last served. A far seek materializes segments past a
hole, so `published_end_ms` can sit behind the playhead while real media is
available ahead of the client.

`delivered_idle_ms` is computed on the VOD arm and deliberately **not
serialized** there: Apple's starvation detector refuses to fire unless the key
is present and at least 16 s, so publishing it would arm an automatic session
reopen for every VOD viewer as a side effect of adding a measurement.

The rolling arm adds the encoder-side detail — `speed`, `recent_speed`,
`out_time_ms`, `progress_idle_ms`, `producer_exit_*`, `hold_reason`,
`resume_below_*`, `readrate`, `suspend_count`, the lease and control fields,
and the client's own reported position and runway.

### 9.4 Teardown

`DELETE /hls/{session}` returns **204** on success and 503 when the release
could not be settled. It is idempotent — deleting a session that has already
gone is a success, because the caller's intent ("this must not be running") is
satisfied either way. There is no 404 arm.

It is capability-authenticated for a specific reason: a browser sends it from
a closing tab with `fetch(…, {keepalive: true})`, and a keepalive request
cannot set headers. Requiring a bearer would make the DELETE undeliverable at
exactly the moment it matters, and the stream would then hold its encoder —
and its hardware slot — for the idle timeout plus a reaper tick with nobody
watching.

### 9.5 How to read the status

**Healthy**: `producer_state: "running"` (or `vod`/`complete` for an
already-materialized title), no `producer_hold`, `admitted: true`,
`ahead_seconds` comfortably positive and growing, `ready_ahead_end_ms` ahead
of `fetched_end_ms`, and on the rolling arm a non-null `delivered_bps` with a
small `delivered_idle_ms`. **`recent_speed` at or above 1.0 is the one to
trust** — the cumulative `speed` can look fine while the recent rate has
collapsed.

**Trouble**: `producer_state: "failed"` flattens every failure to one word, so
`producer_decision` is the only part a client can act on, because it says
whether retrying could ever work. A `hold_reason` is **not** a failure:
`demand`, `time`, `bytes`, `ahead` and `working_set` all lift as the client
consumes what it has; `global` waits on total scratch across every session;
`no_room` is the one nothing the client does will clear. A rising
`suspend_count` with a currently-clear state is a flapping session that
happened to look healthy at poll time. `progress_idle_ms` growing while
`recent_speed` is stale is the decisive signal that the producer has stopped
emitting samples entirely — `-1` there is explicit unknown, not zero. And
`published_end_ms` behind the playhead is not by itself a fault; check
`ready_ahead_end_ms`.

---

## 10. Playback — the control protocol

`POST /api/v1/hls/{session}/control` is one bounded exchange. The session UUID
is the capability; `generation`, `control_epoch`, `client_instance_id` and
`sequence` are mutation fences. Body ≤ **16 KiB**, the whole exchange wrapped
in a **4-second** deadline, minimum interval 250 ms, advertised cadence
5000 ms.

The request reports what the client is doing — `demand` (`active` · `hold` ·
`end`), `position_ms`, `buffered_from_ms` (absent means *no contiguity
evidence*, not a hole), `buffered_through_ms`, `playback_rate`,
`render_state`, `seek_target_ms`, `observed_download_bps` — plus its
`selection`, optional `capabilities` and `observation`, an optional
`acknowledgement`, and `supported_actions`.

Buffer observations and seek intent are independent. A forward or backward
seek may report the old buffer alongside a new `seek_target_ms`, or the new
buffer before `position_ms` catches up; both are accepted, not `invalid_control`.
Both the delivery view and producer pacing count only buffer covering the
destination as runway. When `buffered_from_ms` is absent, coverage is inferred
from the observed position forward, never across a backward seek gap. Numeric
bounds, ordered buffer endpoints, seek-state pairing, and identity fences
remain enforced. A malformed request still returns `400 invalid_control`
with `invalid_field`; the web client retains that field in its error log.

`selection` is `{quality, audio_track?, subtitle, audio_offset_ms, codec,
dynamic_range}`. `quality` is tagged: `{"mode":"auto"}`,
`{"mode":"original"}`, or `{"mode":"manual","height":N}`. `subtitle` is
`{mode, track?}` with `mode` in `off` · `native` · `overlay` · `burn`, where
`off` must carry no track and every other mode must carry one.

`acknowledgement` is `{action_id, state, …}` with `state` in `metadata_ready`
· `buffer_ready` · `committed` · `failed` · `aborted`. `committed` requires
**both** `first_frame_unix_ms` and `committed_media_origin_ms`: between
`prepare` and `committed` the server's intent can move, and an acknowledgement
carrying only an id cannot distinguish a client that committed to the current
offer from one that committed to a stale one. The origin is what makes a
commit evidence.

`supported_actions` is how a client declares what it will apply. **Absent or
empty means passive, and the server may then return only `none`** — it never
sends an action the client did not name. Unknown names are ignored rather than
refused.

The response carries `accepted_sequence`, `server_time_unix_ms`, a `lease`
(`state`, `renew_after_ms: 5000`, `expires_at_unix_ms`), a `delivery` view —
the same vocabulary as the status route, in a shape a client polls anyway —
an `effective_selection`, and one `action`:

| Action | Meaning |
|---|---|
| `none` | The only action a passive client is always safe to receive |
| `hold` | Production is deliberately not advancing, and this is **not** a failure. `revisit_after_ms` is 5000 for every reason except `no_room`, which is 20 000, because nothing the client does clears node capacity. It is a revisit contract, not an expiry: a hold never escalates to a terminal |
| `terminal` | Production stopped for a reason retrying cannot change. Ends the failed production attempt; a 404 or a 410 alone does not authorize discarding a usable buffered player |
| `retry_resource` | Stopped for a reason that may not recur; retry on the server's cadence |
| `prepare` | A successor session is staged, addressed exactly as its own create response would address it. The only action that is a transaction rather than a report, so it is recorded and replayed exactly rather than recomputed |

Production holds do not veto bounded client presentation repair. An active
client explicitly reporting `render_state: stalled` with substantial loaded
runway or published-but-unfetched media receives `none` instead of an advisory
hold, even when decoder state is `ready`, `unknown`, or omitted. This does not
classify the decoder as failed or authorize lowering quality. Intentional
pause and typed permanent producer decisions retain their precedence. The
producer hold remains visible in delivery diagnostics; `none` acknowledges no
server action, not successful presentation recovery.

| Status | Code | When |
|---|---|---|
| 404 | `session_gone` | not a UUID, no durable route holds this capability, or control was never advertised |
| 400 | `invalid_control` | not valid protocol v1, or a field outside the bounded contract (`invalid_field` names it) |
| 409 | `stale_control` | `generation` is no longer current |
| 409 | `owner_changed` | `control_epoch` does not match the route's owner epoch |
| 503 | `control_unavailable` | the route is unreadable, or the 4-second deadline expired (`retry_after_ms: 500`) |

An accepted terminal (`demand: end`) response is retained for 60 seconds and
replayed idempotently.

When the control plane is available it is the better surface than polling
`status`: `hold` versus `terminal` versus `retry_resource` is the server
telling the client directly which of the three it is looking at, rather than
the client inferring it from `producer_state`.

---

## 11. Subtitles and overlays

| Method | Path | Auth | What it does |
|---|---|---|---|
| GET | `/api/v1/files/{id}/subs/{subtitle}` | bearer | One subtitle stream as WebVTT, for a `<track>` element |
| GET | `/api/v1/files/{id}/subs/{index}/overlay.json` | bearer | The `pgs-v1` overlay manifest |
| GET | `/api/v1/files/{id}/subs/{index}/overlay/{generation}/objects/{object}` | bearer | One immutable overlay PNG, addressed by content hash |

`{subtitle}` accepts **both** spellings: the handler strips a trailing `.vtt`
if present and parses the remainder as an integer, so `/subs/0` (legacy) and
`/subs/0.vtt` (the documented sidecar URL) reach the same track. Authenticated
with a bearer, which in practice means `?token=`, because a `<track>` element
cannot set headers.

It validates in order: the segment parses; the file exists; the index
addresses a real subtitle stream — a **positional** lookup, so a negative
index or one past the end is a 404; and the codec is not bitmap.
`hdmv_pgs_subtitle`, `pgssub`, `dvd_subtitle`, `dvdsub` and `xsub` return
**400** with *"this is a bitmap subtitle (PGS/VobSub) and can't be shown as
text; it can only be burned in during transcode"*.

Extraction is cached on disk keyed by (file id, stream index, size, mtime), so
a replaced or re-muxed file misses. A miss writes to a temp name renamed into
place once whole, so two racing misses write identical bytes and the loser's
rename is a no-op. Without the cache, ffmpeg would read the whole source — a
full read of a 60 GB remux across the NAS — every time a player fetched the
track, which is every playback of the same film.

The overlay routes serve PGS subtitles as an application-drawn overlay rather
than a burn: `overlay.json` answers `202 {"state":"preparing"}` while the
build runs, and objects are immutable and addressed by content hash, so they
cache indefinitely.

---

## 12. Offline packages

```
        bearer auth (owner-scoped)
 ┌───────────────────────────────────────────────────────────────┐
 │  GET /files/{id}/offline-options                              │
 │        │  qualities ≤1080p · audio[] · subtitles[]            │
 │        ▼                                                      │
 │  POST /files/{id}/offline-packages   {request_id,height,…}    │
 │        │  202 (or 200 if an identical request is ready)       │
 │        ▼                                                      │
 │  ┌──────────┐  claimed by      ┌────────────┐   published     │
 │  │  queued  │ ───────────────▶ │ preparing  │ ──────────────▶ │
 │  └──────────┘  the encoder     └────────────┘                 │
 │   phase:                        phases, in order:             │
 │   waiting_for_encoder            waiting_for_source           │
 │        │                         validating                   │
 │        │                         transcoding                  │
 │        │                         extracting_subtitles         │
 │        │                         publishing                   │
 │        │                              ▼                       │
 │        │                        ┌──────────┐                  │
 │        │  any phase can fail    │  ready   │ phase: ready     │
 │        ▼                        └────┬─────┘                  │
 │  ┌──────────┐  error_code +          │                        │
 │  │  failed  │  error_message         │                        │
 │  └──────────┘                        │                        │
 │  GET /offline/packages/{id}  ◀───────┤ poll; each poll slides │
 │       (also slides expiry +7 d)      │ the expiry             │
 │  PUT /offline/packages/{id}/lease  {token: 64 hex}            │
 │       201 Created / 200 Renewed ──▶ manifest_url              │
 └───────────────────────────────────────┬───────────────────────┘
                                         │
        package-scoped capability token in the path (no bearer)
 ┌───────────────────────────────────────▼───────────────────────┐
 │  GET /offline/media/{token}/master.m3u8                       │
 │        ├─▶ …/index.m3u8 ─▶ …/segNNNNN.ts                      │
 │        └─▶ …/subs/{i}/index.m3u8 ─▶ …/subs/{i}/seg00000.vtt   │
 │  every read touches last_access_at at most once per 60 s      │
 └───────────────────────────────────────┬───────────────────────┘
                                         │ download verified
                                         ▼
        POST /offline/packages/{id}/complete   ─┐ both delete the row,
        DELETE /offline/packages/{id}          ─┘ its lease and its pin;
                                                 both always return 204
```

| Method | Path | Auth | What it does |
|---|---|---|---|
| GET | `/api/v1/files/{id}/offline-options` | bearer | Rungs ≤1080p, tracks, and recommended indices |
| POST | `/api/v1/files/{id}/offline-packages` | bearer | Idempotent create by `request_id` |
| GET | `/api/v1/offline/packages/{id}` | bearer, owner-scoped | Poll state; also renews the expiry |
| DELETE | `/api/v1/offline/packages/{id}` | bearer, owner-scoped | Cancel and delete. Always 204 |
| PUT | `/api/v1/offline/packages/{id}/lease` | bearer, owner-scoped | Register the client's token against a `ready` package |
| POST | `/api/v1/offline/packages/{id}/complete` | bearer, owner-scoped | Same release as DELETE, named for the lifecycle |
| GET | `/api/v1/offline/media/{token}/master.m3u8` | **capability** | Synthetic multivariant playlist |
| GET | `/api/v1/offline/media/{token}/index.m3u8` | **capability** | The published VOD media playlist |
| GET | `/api/v1/offline/media/{token}/{segment}` | **capability** | One `segNNNNN.ts` |
| GET | `/api/v1/offline/media/{token}/subs/{index}/{segment}` | **capability** | `index.m3u8` or `seg00000.vtt` |

**The auth split is the design.** JSON and package ownership use bearer auth,
and every store call carries the user id, so another user's package id returns
404 rather than 403 — an opaque id leaks nothing about whether it exists. Only
*immutable child media* uses the package capability, because AVFoundation and
Media3 fetch child HLS resources autonomously and cannot be made to attach a
header.

### 12.1 States, and the lease

`state` is exactly four values, constrained by the schema: `queued`,
`preparing`, `ready`, `failed`. Only `ready` can carry a lease or serve media.
`phase` is free text; the worker writes `waiting_for_encoder`,
`waiting_for_source`, `validating`, `transcoding`, `extracting_subtitles`,
`publishing`, `ready`. Failure codes are `source_unavailable`,
`invalid_track`, `encoder_failed`, `subtitle_failed`, `other`.

The client generates its own 256-bit secret and sends `{"token": "<64 hex>"}`;
anything else is 400 `invalid_lease`. The server stores only the SHA-256 of
the decoded bytes, so the raw token never reaches the database and never
reaches a trace. One lease per package: re-sending the same token renews in
place and returns 200, the first registration returns 201, and a *different*
token is 409 `lease_conflict`.

The TTL is **7 days**, and three things slide it: a lease PUT, a status poll,
and any media read — the last throttled to at most one write per 60 seconds,
so a downloader hammering segments touches the row once a minute. When it
lapses, the media routes answer 404 `package_expired`, the same answer as a
token that never existed, and a 60-second sweep deletes the row and its cache
pin. Nothing on the device is touched.

### 12.2 Quotas

Three settings, each floored at 0, each enforced *inside* the insert's `WHERE`
clause rather than by a preceding read:

| Setting | Default | Refusal |
|---|---|---|
| `offline.max_rows_per_user` | 50 rows | 429 `quota_exceeded` |
| `offline.max_gb_per_user` | 15 GiB | 507 `quota_exceeded` |
| `offline.max_gb` | 25 GiB (node-wide) | 507 `insufficient_storage` |

Reservation uses the rung's **peak** bitrate, not the estimate. Two more
create-time refusals are not quotas: 503 `offline_disabled` and 503
`node_removed`.

`offline-options` returns every ladder rung at or below 1080p — labelled
`Standard` at 720 and below, `High` above — with `estimated_bytes` (total
bitrate) and `reserved_bytes` (peak bitrate) per rung, neither of which is a
storage guarantee. Subtitles are marked `offline_mode: "native"` or
`"unavailable"`; there is no burn-in mode here. The recommended subtitle index
is emitted only when it is a native text codec, so it is never a track the
create route would refuse.

### 12.3 Media routes

All four responses carry `private, max-age=604800, immutable` — 7 days,
matching the lease. `master.m3u8` is synthesised from the row but still opens
the package directory first, so an evicted artifact fails at the root request
rather than one child later. `index.m3u8` is verified on read and a missing or
non-VOD playlist is 410 `package_corrupt`. Segment names must match
`segNNNNN.ts` exactly. The subtitle route accepts exactly two names,
`index.m3u8` and the literal `seg00000.vtt`, and re-extracts a pruned sidecar
only when the current file row still names the same path, size and mtime the
package snapshotted — otherwise 410 `source_changed`.

### 12.4 How to read a package

A healthy download is a short trajectory: 202 on create;
`queued`/`waiting_for_encoder`; then `preparing` with `phase` advancing and
`progress` climbing monotonically; then `ready` with `actual_bytes` and
`duration_ms` populated. From there the lease returns 201 once and 200 on
every retry with the same token.

What is stuck: a row that stays `queued`/`waiting_for_encoder` across many
polls has no encoder claiming it — that is capacity or a paused node, not a
client problem. A row in `preparing` whose `progress` has not moved between
polls is a wedged transcode, and `phase` names the stage. `failed` is terminal
and nothing retries it; the client must create a new package with a fresh
`request_id`. On the media side: 404 `package_expired` means the lease aged
out or the sweep ran (recreate); 409 `package_not_ready` means the client
leased too early; 410 `package_corrupt` means the published artifact failed
integrity verification and must be prepared again; and 409 `lease_conflict`
means the client lost its stored secret — since the server never rotates a
valid URL, the only way forward is to release and start over.

---

## 13. Books — publications and reading state

| Method | Path | Auth | What it does |
|---|---|---|---|
| GET | `/api/v1/files/{id}/content` | bearer | Original book bytes, with range support |
| POST | `/api/v1/files/{id}/publication` | bearer | Parses an EPUB, returns a manifest, mints a session |
| GET | `/api/v1/publication/{session}/{*resource}` | **capability** | One bounded EPUB archive entry |
| DELETE | `/api/v1/publication/{session}` | bearer, owner-scoped | Closes a session early |
| GET | `/api/v1/items/{id}/reading-state?file_id=` | bearer | `{state, stale}` |
| PUT | `/api/v1/items/{id}/reading-state` | bearer | Saves a revision-bound locator; body ≤ 64 KiB |
| DELETE | `/api/v1/items/{id}/reading-state?file_id=` | bearer | Clears it. 204 |

`POST …/publication` requires a `book` item and a `.epub` file, then parses
synchronously: verify the file's size and mtime still match the row, then walk
`META-INF/container.xml`, the OPF package, and the nav document or NCX into a
Readium-shaped manifest. It returns `{session_id, resource_base, expires_in,
file_id, revision, publication, limits}`.

`limits` publishes the parser's own bounds so a client can *explain* a
refusal rather than just showing one: 20 000 entries, 1 GiB total
uncompressed, 128 MiB per resource, 8 MiB of markup, a compression ratio of
1 000, 8 concurrent resource reads, 64 KiB chunks.

`{*resource}` takes **no** auth extractor — the session id is the whole
credential, so a sandboxed iframe never receives the user's API token. The
requested path is normalized and must be a key in the session's declared
resource map, so only entries the manifest actually declared are reachable.
Every read re-verifies the file's size and mtime, takes one of 8 permits, and
streams in 64 KiB chunks; if the streamed length disagrees with the ZIP
directory's declared size in *either* direction the stream errors rather than
truncating silently. Responses are locked down: `Cache-Control: no-store`,
`nosniff`, `no-referrer`, `same-origin`, and a CSP beginning
`default-src 'none'; script-src 'none'; connect-src 'none'`.

Sessions are in-memory only and die three ways: an explicit DELETE (which
matches on the owning user); a **sliding** 2-hour TTL, pushed forward by every
successful resource read, so a session dies two hours after its last read
rather than after opening; or eviction under pressure at 64 sessions, oldest
*touched* first.

### 13.1 Reading state

`PUT` takes `file_id` (a number **or** a decimal string, because JavaScript
cannot hold the full i64 range), `revision: {size, mtime}`, `locator`,
`progression`, `completed`, and an optional `recorded_at`.

Validation, in order: the item must be a book; the file must belong to it; the
revision must equal the current file row (409, *"the book file changed; reload
it before saving reading state"*); `progression` must be finite and within
`[0,1]`; and the locator must be a version-1 object whose `href` is a
normalized relative publication path — no leading slash, no backslash, no
control characters, no percent-encoded traversal, no empty or dotted
segments, at most 2 048 bytes.

Two limits, not one: the route carries a 64 KiB body limit, and the serialized
locator itself may not exceed 32 KiB (413 `locator_too_large`).

Conflicts resolve in two layers. A **revision** conflict is a 409 on `PUT` and
nothing is written; on `GET` it is not an error at all — the response is
`{"state": null, "stale": true}`, so a reader knows to reopen rather than
resume into a stale locator. A **recency** conflict between two devices is
resolved by the same policy watch progress uses: a live write (no
`recorded_at`) always wins, and a dated offline write older than the stored
row updates nothing and returns the existing winning row.

---

## 14. Watch state

| Method | Path | Auth | What it does |
|---|---|---|---|
| POST | `/api/v1/items/{id}/progress` | bearer | Reports a position; returns the winning watch row |
| POST | `/api/v1/items/{id}/scrobble` | bearer | Marks watched, cascading to every descendant |
| POST | `/api/v1/items/{id}/unscrobble` | bearer | Marks unwatched and clears progress, same cascade |

`progress` takes `{position_ms, duration_ms?, recorded_at?}` and has two write
paths. **Without** `recorded_at` — a live player's beat — the write is
coalesced, and the response reports the coalescer's view; it also touches the
direct-play registry, which is the only signal that a direct-play viewer who
paused with the film buffered still exists. **With** `recorded_at` — an
imported or offline fact — the write carries its own ordering clock and is
admitted only if it is not older than the stored row, so an old fact changes
nothing rather than rewinding newer playback.

Crossing 95 % of duration flips `watched`, once. The handler reads the prior
value first so it can distinguish "already watched" from "just became
watched" and notify only on the crossing, and `watched` is monotone in the
upsert, so a later beat at 10 % does not un-watch.

`scrobble` and `unscrobble` take **no body** and act on the whole item tree:
marking a show marks every episode underneath, because marking only the
container would leave Next Up offering episode one of a series you just said
you had seen. Both return `{"ok": true, "updated": <count>}` where the count
is the number of descendants that actually *changed* — re-marking a finished
series returns 0 and notifies nothing.

---

## 15. Dolby Vision disk conversion

Every route here is admin.

| Method | Path | What it does |
|---|---|---|
| PUT | `/api/v1/libraries/{id}/dv-conversion` | Sets that library's mode |
| POST | `/api/v1/libraries/{id}/dv-conversions` | Queues a bounded batch. 202 |
| GET | `/api/v1/files/{id}/dv-conversion` | Eligibility, probed DV metadata, ledger row, tool capabilities |
| POST | `/api/v1/files/{id}/dv-conversion` | Queues one file |
| GET | `/api/v1/dv-conversions` | Fleet progress and recovery guards, or a per-file ledger page |

Mode is exactly three literals: **`off`** (the default — queueing is refused
with 409 and the message *"library Dolby Vision conversion mode is Off"*),
**`manual`** (only when an operator asks), **`auto`** (the daemon queues
eligible files itself). Anything else is a 400 naming all three.

A file is eligible only when all five hold: the container is `mkv`; the DV
profile is 7; `bl_compat_id` is 1 or 6; an enhancement layer is present; and
an RPU is present. Queueing maps the store's outcome onto 202 `queued`, 200
`already active`, 409 for an already-committed file that cannot be re-queued,
and 422 with the reason for anything else ineligible. A missing `dovi_tool`
or `mkvmerge` is a 503.

Ledger rows carry a state of `queued` · `running` · `verified` · `committed` ·
`failed`, plus `el_type`, `original_path`, byte counts before and after, the
error, timestamps, and an optional recovery guard.

`GET /api/v1/dv-conversions` has two shapes. With `?file_ids=` (comma
separated, at most **256** ids and 6 KiB of raw input, each malformed token
its own 400) it returns per-file ledger rows and eligibility. Without it,
optionally narrowed by `?library_id=`, it returns tool `capabilities`, every
library's mode, the `keep_original` and `parallel` settings, `progress`
(`eligible`, `queued`, `running`, `verified`, `committed`, `failed`),
`progress_by_library`, and `recovery_guards`. Progress and guards are read in
one join so the counts and the page come from a coherent snapshot, and
`orphans_truncated` makes a large orphan set visible as a count even when the
page is capped at 256.

---

## 16. The analysis queue

Every route here is admin.

| Method | Path | What it does |
|---|---|---|
| POST | `/api/v1/files/{id}/analysis` | Durably requests analysis for one or both components. 202 |
| GET | `/api/v1/analysis/summary` | Counters plus a queue-health verdict |
| GET | `/api/v1/analysis/jobs` | Keyset history page with server-side filter and search |
| GET | `/api/v1/analysis/jobs/{id}` | One job's exact durable state and attempt history |
| DELETE | `/api/v1/analysis/jobs/{id}` | Cooperative cancellation |
| POST | `/api/v1/analysis/jobs/{id}/retry` | Creates a successor for one terminal job |
| POST | `/api/v1/analysis/reopen` | Bulk reopen; **dry-run by default** |
| PUT | `/api/v1/files/{id}/timeline-annotations/{kind}` | Writes a durable manual boundary |
| DELETE | `/api/v1/files/{id}/timeline-annotations/{kind}` | Discards it, with explicit confirmation |

### 16.1 Two vocabularies

Storage states are `queued`, `running`, `submitted`, `ready`, `failed`,
`cancelled`. **Clients should read `durable_state`, not `state`** — the raw
storage word will mislead you:

| `durable_state` | Means |
|---|---|
| `stale` | the source was superseded — wins over everything else, and is not a failure |
| `queued` | waiting, and claimable now |
| `retry_wait` | queued, but serving a backoff (`not_before_ms` is in the future) |
| `claimed` | a worker holds it |
| `staged` | produced, not yet published to the target node |
| `published` | done |
| `failed` | terminal failure |
| `canceled` | cancelled (note the single-l spelling on the wire) |

`GET /analysis/jobs` accepts a comma-separated `state=` subset and normalizes
the aliases (`running`→`claimed`, `ready`→`published`,
`cancelled`→`canceled`); an unrecognized value is a 400. `filter=` is `all` ·
`working` · `attention` · `ready` · `expected`; `q=` is capped at 120
characters; `limit=` defaults to 25 and is echoed back clamped to `10..=100`;
`cursor=` is the opaque triple from `next_cursor` and is validated on the way
in.

Each row carries, among others, `job_attempt_errors` — the code each charged
attempt ended with, oldest first. That field exists because the terminal
`job_error_code` for an exhausted budget is always `attempt_limit`, which
names no cause. Fragment-index rows may also include `index_diagnostic`,
`index_retry_deadline_ms`, `effective_retry_at_ms`, and `terminal_reason`.
The diagnostic is a bounded versioned object: unknown versions and invalid or
oversized records appear as absent instead of leaking untrusted store text.

### 16.2 The health verdict

`GET /analysis/summary` returns counters plus `health`, and `health` is
**absent until this node has taken its first store sample** — a verdict with
nothing behind it would read as `idle`, which is a claim.

| Verdict | Means |
|---|---|
| `idle` | nothing ready, claimed, claimable or past lease in 24 h. A fully indexed library and a paused one look identical from here, and neither is a fault |
| `healthy` | work is going in and coming out |
| `degraded` | work is going in and not coming out, but not enough of either to be sure; or lease losses are a quarter of claims; or attempt-limit failures are a tenth of them; or two consecutive samples show work running past its lease |
| `dead` | nothing produced in 24 h **and** either at least 20 claims in that window, or zero claims with a claimable backlog. A queue that is running and producing nothing |

### 16.3 `retry` versus `reopen`

`retry` is the single-row operator override: the job must be exactly `failed`
or `cancelled`, it mints one successor, it kicks the queue, and it performs
**no** headroom check.

`reopen` is the bulk form, for the shape retry is wrong for — a queue that
failed every job it claimed for days, leaving four figures of terminal rows
because the *queue* was broken and not the sources. It is deliberately awkward
in four ways:

- **`dry_run` defaults to `true`.** An empty body reports and changes nothing,
  because the destructive reading of an empty body is the one nobody asked
  for.
- **`limit` counts distinct source files, not requests** — a file with a
  failed fragment index and a failed marker pass is one unit. Default 50,
  bounded at 500.
- **It respects a headroom floor of 512 active slots**, so discovery, the
  foreground enqueue that playback does, and the single-row Retry are not
  taken down with it.
- **It skips loudly rather than refusing**: a row whose successor cannot be
  created is counted in `skipped_unavailable` and left to discovery.

Candidates are counted only while no request for the same source, component
pipeline version, video identity, and target is already live, which is what
makes it safe to press twice — a
file repaired on Monday is not re-forced on Friday. Watch
`stopped_at_headroom` and `scan_truncated` on the response: either means more
work is waiting and the call must be repeated.

The selected-video completion repair uses the same route with
`"repair_revision":"video-completion-v1"` and component `fragment_index`.
Its preview returns exact candidate descriptors, eligibility, stable candidate
IDs, a continuation cursor, receipt counts, and skipped reasons. It reads at
most 1,500 pipeline candidates and the ordinary 50/500 distinct-file limit
still applies. Apply must send those exact descriptors back with
`"dry_run":false`; the Store treats them as untrusted and atomically rechecks
the source, predecessor fence/update time, pipeline identity, current head,
active successors, receipt, and 512-slot headroom. A stale descriptor is
reported, never replaced by a newly selected file. Repeating or racing apply
returns the durable successor from the once-per-source/pipeline receipt.

### 16.4 Timeline annotations

`{kind}` is exactly `intro` · `recap` · `credits` · `preview`. `PUT` takes all
five of `start_ticks`, `end_ticks`, `timescale`, `start_ms`, `end_ms`, stamps
provenance `manual`, and requires the file to have a measured duration.

**What a manual annotation overrides is the automatic detector**: once a
manual row exists, a reanalysis, a pipeline-version bump or a rebuild cannot
replace or move it. It is bound to the file's source identity, so a change to
the underlying media does invalidate it (409).

`DELETE` is deliberately awkward too: the body must carry both the current
`revision` and `"confirm_discard_manual_override": true`. A missing or false
confirmation is a 400; a stale revision is a 409.

### 16.5 How to read the queue

Read `durable_state` per row together with the summary's `health`. A healthy
queue has rows moving `queued → claimed → staged → published`, a small
`working` count against `total`, and attempts in the low single digits.

What is stuck: rows in `claimed` whose `lease_expires_ms` is behind `now_ms`
mean a worker died holding the claim. A large `retry_wait` population with
`not_before_ms` far ahead is a queue *serving backoff*, not a queue working —
read `job_attempt_errors` for what the attempts actually ended with. `staged`
rows that never reach `published` mean the artifact exists but is not on
`target_node_id`. `stale` rows are not failures at all. The unambiguous alarm
is `health.verdict == "dead"`, and the fix — once the underlying cause is gone
— is `POST /analysis/reopen`, dry run first.

---

## 17. Live TV

```
 ADMIN                    ANY USER                     OWNER NODE
   │ GET  /live-tv/readiness │                   (config.owner_node_id)
   │ POST /live-tv/readiness/refresh                        │
   ├────────────────────────────────────────────────────────▶ discover.json
   │◀── verdict + snapshot ─────────────────────────────────┤ lineup.json
   │                         │                              │ graph probe
   │              GET /live-tv/channels                     │
   │                         ├─────────────────────────────▶│ snapshot cache
   │                         │◀── freshness, age_seconds ───┤ (30 s fresh,
   │                         │    channels[]                │  5 min stale)
   │       POST /live-tv/channels/{channel}/sessions        │
   │                         │  ┌ phase 1: start ──────────▶│ forced lineup
   │                         │  │  ≤17 s/attempt, ≤24 s     │ refresh, pick
   │                         │  │◀ capability + activation ─┤ tuner, spawn
   │                         │  │   token                   │ ffmpeg
   │                         │  │ ┌ phase 2: activate ─────▶│ provisional
   │                         │  │ │  5 s                    │ expires in 40 s
   │                         │◀─┴─┘ 200 {session_id,        │ if not activated
   │                         │       playlist_url, …}       │
   │              ┌─ GET  .../index.m3u8 ──────────────────▶│ ≤6 segments
   │              │  GET  .../segment-000001.ts ───────────▶│ listed
   │              │  GET  .../status ─────────────────────▶ │ each of these
   │              └─ PUT  .../keepalive ──────────────────▶ │ sets last_touch
   │              DELETE /live-tv/sessions/{capability}     │ cancel, kill
   │                         │                              │ ffmpeg, close
   │  no DELETE? now - last_touch ≥ 45 s ───────────────────▶ the tuner GET
   │  (checked on a 250 ms tick) → identical teardown, then 410 Gone
```

| Method | Path | Auth | What it does |
|---|---|---|---|
| GET | `/api/v1/live-tv/readiness` | admin | Verdict from cached device state |
| POST | `/api/v1/live-tv/readiness/refresh` | admin | Verdict, forcing a fresh fetch and re-probing the encoder graph |
| GET | `/api/v1/live-tv/channels` | bearer | The sanitized lineup |
| GET | `/api/v1/live-tv/guide` | bearer | The cached programme guide, clipped to `?from=<unix>&hours=<1..336>`. Never triggers a fetch. Carries `next_refresh_at` so a client polls on the owner's clock |
| POST | `/api/v1/live-tv/guide/refresh` | admin | Forces one guide refresh on the owner and returns the new document |
| GET | `/api/v1/live-tv/guide/readiness` | admin | Advisory: what has to be true for the configured source to work, and whether it is |
| POST | `/api/v1/live-tv/channels/{channel}/sessions` | bearer | Two-phase start; issues the capability |
| GET | `/api/v1/live-tv/sessions/{capability}/index.m3u8` | **capability** | Live media playlist |
| GET | `/api/v1/live-tv/sessions/{capability}/{segment}` | **capability** | One MPEG-TS segment |
| GET | `/api/v1/live-tv/sessions/{capability}/status` | **capability** | Session state |
| PUT | `/api/v1/live-tv/sessions/{capability}/keepalive` | **capability** | 204; renews the idle timer and nothing else |
| DELETE | `/api/v1/live-tv/sessions/{capability}` | **capability** | Releases the session and the tuner |
| DELETE | `/api/v1/live-tv/starts/{request_id}` | bearer | Retires the client's own start id: stops whatever it produced and fences it. `{"outcome": "stopped" \| "ended" \| "retired"}` |
| POST | `/api/v1/live-tv/starts/{request_id}/resume` | bearer | Rejoins the session that id still owns. `{"outcome": "live", "session": …}`, or `pending` / `ended` / `retired` with no session |
| GET | `/api/v1/live-tv/starts/{request_id}` | bearer | What became of that start: `starting` \| `active` \| `ended` \| `retired` \| `unknown`. Never a capability |

The three `starts/{request_id}` routes are the opposite shape: they take the
account bearer and no capability, because after an unclean end the client's
own request id is the only handle it still has. The id is redacted from the
access log all the same — it derives a capability through replay and resume —
and the retire and resume stay eligible during node maintenance, for the same
reason the session DELETE does: a viewer must be able to let go of a tuner
precisely when the node is being worked on.

### 17.0 Recording

The DVR mutation routes write intent; they do not open tuners or files. The
owner loop reads those replicated rows and is the only writer of `recording`
and terminal states. Runtime reads join the durable row to a bounded owner
observation. `DELETE` on a live recording therefore answers `202 {pending:
true}`: acceptance is visible as `Stop requested`, and closure is a later
owner fact.

| Method | Path | Auth | What it does |
|---|---|---|---|
| GET | `/api/v1/dvr/status` | bearer | Switch, root, free space against the floor, tuner slots and reserve, next start |
| GET | `/api/v1/dvr/overview` | bearer | Bounded versioned foreground projection: exact counts, at most 64 active rows, owner observation age, next capture and server-calculated capabilities. `availability` is `complete`, `partial` or `unavailable`; missing counts are `null`, never an invented zero. Admins alone receive diagnostics |
| GET | `/api/v1/dvr/recordings` | bearer | Rows; `?state=` filters by name, `?after=` and `?limit=` page. Excludes `deleted` unless named |
| POST | `/api/v1/dvr/recordings` | bearer | Record one airing: `{channel_id, airing_start}` copies the programme from the guide, or `{channel_id, capture_start, capture_end, title}` records a fixed span with no guide at all |
| GET | `/api/v1/dvr/recordings/{id}` | bearer | One row, with `item_id`/`file_id` once the scan has linked it |
| GET | `/api/v1/dvr/recordings/{id}/events` | bearer | Newest lifecycle events first with `?before=&limit=`; mutually exclusive `?after=` returns ascending incremental events. Carries `next`, `history_complete` and `truncated_before_sequence` |
| POST | `/api/v1/dvr/recordings/{id}/attention/ack` | bearer | Monotonically marks one recording reviewed through `{"through_sequence":N}` for this user. It changes neither media nor recording state |
| DELETE | `/api/v1/dvr/recordings/{id}` | bearer | Planned → cancelled, durably. Recording → records the stop request, `202 {pending}`. Finished → deleted with its file, and only with `?delete_file=1` |
| POST | `/api/v1/dvr/recordings/{id}/restore` | bearer | Cancelled → scheduled. The only way back; rule expansion never does it |
| GET | `/api/v1/dvr/schedule` | bearer | Legacy `?days=<1..14>` remains. Windowed readers default to now −12 h through now +12 h and may supply `from`, `to` (at most 24 hours), repeated `channel_id` (at most 64), `after`, and `limit` (at most 100); rows order by `(capture_start,id)`, `next` pages, and `conflicts` remains exact for the whole requested scope |
| GET | `/api/v1/dvr/attention` | bearer | Per-user paginated projection with `rows`, opaque `next` and exact `total`; unresolved current conditions precede unreviewed historical outcomes. The cursor carries its section, exclusive key and both first-page upper watermarks, so new incidents appear on a refreshed first page rather than inside an in-progress traversal |
| GET | `/api/v1/dvr/rules` | bearer | Series rules, in priority order |
| POST | `/api/v1/dvr/rules` | bearer | Create; `from_airing` fills mode, value and channel from a guide cell |
| PUT | `/api/v1/dvr/rules/order` | admin | Renumber every rule. The order decides who gets a tuner when two rules want one |
| PUT | `/api/v1/dvr/rules/{id}` | bearer (owner or admin) | Edit |
| DELETE | `/api/v1/dvr/rules/{id}` | bearer (owner or admin) | Delete. Its pending airings are withdrawn on the next tick — not cancelled, because that is the viewer's word |
| GET | `/api/v1/dvr/reminders` | bearer | The caller's reminders; `?due=1` returns only fired ones, each saying whether a recording already covers it |
| POST | `/api/v1/dvr/reminders` | bearer | `{channel_id, airing_start, lead_s?}`. Works with recording switched off — a reminder needs no tuner |
| DELETE | `/api/v1/dvr/reminders/{id}` | bearer | Own only |
| POST | `/api/v1/dvr/reminders/{id}/ack` | bearer | Fired → acked, so a second device does not show it again |

Typed error codes here include `dvr_disabled`, `airing_unknown`, `airing_past`,
`rule_limit`, `reminder_limit`, `delete_file_required` and
`invalid_attention_sequence`. Malformed history, attention and schedule
cursors are typed 400 responses rather than empty pages.

Runtime observation expires after 20 seconds. `attempt_bytes_written` and
`write_bps` count successful application writes in the current attempt; they
are not decoded-video, filesystem-sync or playable-duration claims. A
`total_bytes_written` value exists only when the owner knows the distinct
prior-attempt baseline. Every response in this block is private and
`no-store`.

The session routes take no account bearer at all, and an `Authorization`
header on them is ignored. Two consequences follow: their capability values
are redacted from the access log, and they stay eligible during node
maintenance and on a non-voting learner while the lineup read and new starts
do not — an in-flight viewer keeps playing through a maintenance window, and
nobody new gets a tuner.

### 17.1 The tuner is a singleton, and the capability names its owner

One node is the tuner owner. Every other node is an ingress that proxies
start, activate, resource and stop over the internal RPC surface. The owner is
named **inside** the capability:

```
    ltv1.<base64url(owner_node_id)>.<uuid-v4>
```

so any node can route any session without a lookup, and no node can serve a
capability it does not own. The parser requires exactly three parts, the
literal `ltv1`, a v4 UUID, and a middle segment that re-encodes to the
identical string — there is exactly one spelling of any capability.

Owner **change is a drain, not a takeover**: the new owner must obtain a
signed acknowledgement from the old one, bound to a nonce, both node ids and
the drain generation. Until that lands, the `owner_transition` readiness check
is red and nothing starts. There is deliberately no timeout-based takeover,
because elapsed time cannot prove the old ffmpeg closed its tuner socket.

### 17.2 Readiness

`GET` reads cached device state; `POST …/refresh` bypasses the snapshot cache
and ignores the graph probe's 6-hour TTL. A forced refresh takes one of 8
permits; a ninth concurrent caller gets `device_unavailable`.

The body is `{ready, enabled, owner_node_id, generation, checks[], snapshot?}`
where each check is `{id, ready, message}`. The checks run in a fixed order,
and the first four are unconditional — if `configuration`, `serving_authority`
or `owner_transition` fails, the response has those four and nothing else, and
**the device was never contacted**, so the absence of `owner_network` is not
evidence about the tuner.

| Check | Red means |
|---|---|
| `configuration` | A saved setting is invalid — session limit outside 1–4, output height not 720 or 1080, a blank owner, a half-written owner-transition barrier, or an HDHomeRun address that is not private or link-local unicast IPv4. plurx will not be pointed at a routable or cloud-metadata address |
| `cluster_protocol` | A node in the cluster runs a build without the live-TV protocol. It is named. This check does *not* short-circuit the device probe |
| `serving_authority` | The node you are asking has lost quorum serving authority. Nothing media-related will start here |
| `owner_transition` | The previous tuner owner has not acknowledged cleanup. Bring it back, or perform the explicit physical-fencing attestation in Live TV settings |
| `owner_network` | The owner could not reach the device, or is not a reachable committed voter, or the device answered something invalid. Messages never contain the device URL by construction |
| `lineup` | The lineup is empty (scan on the device), or it is `Stale` — the fetch failed and the owner is serving a projection up to 5 minutes old |
| `session_limit` | plurx is configured to lease more tuners than the device reports |
| `ffmpeg_graph` | This owner's ffmpeg cannot produce the required output. No channel will start until it is green. "Never probed" is its own message, not a failure |
| `drm_boundary` | Never red. It states the product boundary and is excluded from the verdict, so it can never be the reason |

**How to read it:** read `ready` last. It is a conjunction, so one `false`
anywhere sets it, and the useful information is *which*. Walk `checks` from
the top and stop at the first red — the order runs from cheapest and most
local to most remote, so the earliest red is nearly always the cause and the
rest are consequences. `enabled` is independent in both directions:
`enabled: false, ready: true` is a correctly configured server waiting to be
switched on, and `enabled: true, ready: false` is a server that will refuse
every start for the reason named. When every check is green and playback still
fails, the verdict has done its job — the next surface is the per-session
`status`.

### 17.3 The lineup

`GET /api/v1/live-tv/channels` refuses twice before any device work: 503
`live_tv_disabled`, and 503 `live_tv_protocol_unready` when any cluster node
lacks the protocol. It never forces a device fetch and never exercises the
encoder.

The response is `{freshness, age_seconds, last_success_at, refresh_error?,
channels[]}`, where `freshness` is `fresh` (fetched within 30 s) or `stale`
(the refresh failed and this is the last good projection, up to 5 minutes
old). Each channel is `{id, guide_number, guide_name, favorite, drm, support,
hd?, video_codec?, audio_codec?}` where `support` is `ready` or
`drm_unsupported`. The three format fields are the tuner's lineup claims:
`hd` is a Boolean, and codec names are trimmed strings of at most 32 bytes.
An absent or malformed format field is omitted, not guessed from the virtual
channel number or from whether the channel is ATSC 1.0 or 3.0.

Nothing else from the device's lineup survives. In particular, these fields
describe the broadcast source, not the stream plurx delivers. The device URL
is validated and then **discarded** — a client never receives one, and the
server never follows a lineup-supplied URL.

**DRM channels are marked, not hidden.** A household that can see channel 2.1
on the TV should see why plurx will not play it rather than wonder where it
went. The DRM marker fails **closed**: `true`/`1` is protected, `false`/`0`
and absent are not, and *anything else* — a string, a null, an unfamiliar
number — is protected. The favorite marker uses the same parser with
fail-*open*, because a field that decides sort order is not a field that
decides whether to open a tuner.

The refusal is enforced again at start, not only in the lineup: a start
re-fetches the lineup forced, so it never rides the stale projection, and a
protected channel is 415 `drm_unsupported`.

### 17.4 Session status separates source, delivery and reception

`GET /api/v1/live-tv/sessions/{capability}/status` returns
`{state, channel, owner_node_id, encoder, output_height, media_sequence,
age_seconds, idle_seconds, signal?, error?}`. `channel` carries the source
format fields from §17.3; `encoder` and `output_height` describe plurx's
H.264/AAC delivery stream.

When the device exposes `/status.json` and names a busy tuner row for the
session's virtual channel, `signal` is
`{strength_percent?, quality_percent?, symbol_quality_percent?}`. All three
values are the device's normalized `0..100` readings. Strength is received RF
level; quality is the tuner's signal-quality reading; symbol quality reflects
whether demodulated symbols are arriving without errors. Read them together:
high strength with low quality can still be a bad signal, while symbol quality
below 100 means the tuner is seeing errors.

Signal is diagnostic and best-effort. It is cached for 4 seconds, a missing or
unsupported `/status.json` answer is cached for 15 seconds, and either case
never fails playback or the status request. The `signal` object is omitted
when the device cannot supply a bounded matching row; absence means
*unavailable*, not zero. The endpoint is capability-authenticated and the
device address, tuner resource and target IP never leave the owner.

### 17.5 The capability's three clocks

All enforced on a 250 ms tick:

- **Provisional, 40 s** — a start that is published but never activated dies.
  Sized to exceed the controller's 24 s start plus two 5 s activation
  exchanges, so a lost activation response cannot strand a tuner.
- **Idle, 45 s** — once activated, 45 s without a touch ends the session.
  Playlist, segment and status reads all touch it, so a client genuinely
  fetching segments never needs the keepalive; the PUT exists for the paused
  case. The web client heartbeats every 10 s and deliberately does *not*
  keepalive a hidden tab, so a backgrounded tab drops its tuner in 45 s.
- **Producer progress, 30 s** — independent of the client. If the encoder
  stops advancing the playlist the session ends. The start budget is separate
  and split: 15 s while the tuner has delivered *zero* bytes ("the channel may
  have no signal"), 30 s once bytes are flowing.

The keepalive extends `last_touch` and nothing else. It cannot revive an
expired session — the handler checks terminal state, cancellation and
activation *before* touching the clock, so a keepalive against a dead session
is 410 `capability_expired`, not a resurrection.

`DELETE` is idempotent by construction: a well-formed capability with no live
session succeeds, so a retried delete is 204. A malformed capability is 410.
Idle expiry performs exactly the same teardown; the difference is only who
started it and 45 seconds of tuner time.

Segments are `segment-<digits>.ts`, at most 10 digits; anything else is a 404
before any session lookup. The published window is at most 6 segments and a
request outside it is 410. At most 4 segment responses stream concurrently per
session; the fifth is 502.

Error codes: `invalid_request` (400), `channel_not_found` (404),
`request_timeout`/`startup_timeout` (408), `settings_conflict` (409),
`capability_expired` (410), `drm_unsupported`/`codec_unsupported` (415),
`stream_failed` (502), and `live_tv_disabled`, `tuner_capacity`,
`tuner_unavailable`, `device_unavailable`, `owner_unavailable` (503).

---

### 17.5 Library channels — a deterministic schedule over finite media

Library channels use the same finite HLS session service as ordinary VOD, but
only through the dedicated session route below. The server resolves the
effective immutable generation from its UTC clock, verifies the pinned file
fingerprint, supplies the source offset, and binds the complete typed worker
recipe to the durable request identity before producer placement. Activation
requires that exact pre-placement record, and the canonical media-session
recipe carries it through ownership transfer and recovery. Following
sessions do not consume the normal playback-start/history notification; an
ordinary session for the same user and item remains ordinary VOD.

Every response under this prefix is `Cache-Control: private, no-store`.
Personal-channel IDs that are not the caller's are 404. Shared definitions are
visible to signed-in users; only their owner or an administrator may mutate
them, and only an administrator may publish shared visibility.

| Method | Path | Auth | What it does |
|---|---|---|---|
| GET | `/api/v1/library-channels` | bearer | Visible channel summaries with private favourite state and derived now/next; `management=true` is admin-only |
| POST | `/api/v1/library-channels/subject-previews` | bearer | Persist a subject preview; 202 acknowledgement with `job_id`, state, counts and stable `preview_seed`; body carries `recipe`, `request_id`, optional `preview_seed` |
| GET | `/api/v1/library-channels/subject-previews/{id}` | owner/admin | Poll state and at most 50 named decisions; `verdict=match\|no_match\|uncertain` and opaque `cursor` paginate; item IDs are decimal strings |
| DELETE | `/api/v1/library-channels/subject-previews/{id}` | owner/admin | Cancel preview work; saved-channel work and cached decisions survive |
| POST | `/api/v1/library-channels/preview` | bearer | Bounded recipe evaluation without file I/O or publication; accepts an opaque `cursor` plus reusable `preview_seed`, and returns `next_cursor`, `first_ten`, diagnostics and the effective seed |
| POST | `/api/v1/library-channels` | bearer | Idempotently persist a definition (201; replay 200); queue publication and return without inference |
| GET | `/api/v1/library-channels/{id}` | bearer | Definition, revision, generation pointers, and mutation capabilities |
| PUT | `/api/v1/library-channels/{id}` | bearer | Full expected-revision replacement; the working schedule stays active until the next rotation |
| DELETE | `/api/v1/library-channels/{id}` | bearer | Idempotent deletion with mandatory `expected_revision` and `request_id` query parameters; media is never deleted |
| POST | `/api/v1/library-channels/{id}/rebuild` | bearer | Rebuild with next-rotation or next-programme activation and optional reshuffle |
| GET | `/api/v1/library-channels/{id}/build` | bearer | Durable queued/building/ready/failed state, bounded error/count facts, active/pending generations, attempts, success and activation time |
| PUT | `/api/v1/library-channels/{id}/favourite` | bearer | Idempotently set the caller's private favourite |
| GET | `/api/v1/library-channels/guide` | bearer | At most 20 channels and 24 hours, capped at 1,000 derived occurrences; continue with the opaque cursor in `X-Plurx-Next-Cursor` |
| POST | `/api/v1/library-channels/{id}/resolve` | bearer | Resolve server-now only; opens no file and creates no session |
| POST | `/api/v1/library-channels/{id}/sessions` | bearer | Revalidate an occurrence and create one following finite-HLS session |

Recipe version 1 gains optional `subject`: trim, NFC normalization, at most 500
Unicode scalar values. Creation with an absent, null or empty subject keeps
ordinary rules. On update an **absent** subject preserves the stored subject;
explicit null or empty clears it. Advanced filters remain AND constraints;
explicit includes bypass subject matching but never eligibility, scope or an
explicit exclusion. New editors always serialize clearing. `subject_recipe`
on channel detail mirrors the recipe with decimal-string identifier arrays for
browser-safe round trips; ordinary numeric arrays remain accepted.

Saving and enabling require no preview or provider readiness. Subject work is
`queued`, `running`, `waiting_for_provider`, `complete`, `failed`, `cancelled`
or `superseded`. Progress is available as `matching` on owner/admin detail and
build responses. Polling returns total/processed/matched/rejected/uncertain,
`result_revision`, a bounded error, and named decision reasons/evidence. A
changed results/catalogue cursor returns 409 `subject_cursor_restart`; missing,
expired or unauthorized jobs return 410 `subject_preview_gone`. Preview leases
expire after 24 hours; restarting a preview reuses exact cached decisions.
Only `match` or explicit inclusion admits a title. Incomplete/provider errors
never become negative decisions. Existing rotations remain playable throughout
classification; publication retains normal next-rotation/next-programme rules.

The collection's canonical spelling has no trailing slash. The server also
accepts `/api/v1/library-channels/` for list and create during the first native
client rollout; new clients must use the canonical route.

`resolve` returns the channel and definition revision, generation, cycle and
ordinal, server time, half-open start/end boundaries, pinned item/file, source
position, and explicit capabilities. The session call repeats the generation
and occurrence plus a monotone client `tune_sequence`; it nests the existing
finite-HLS create body under `playback`. Its response is
`{playback: <StartResponse>, library_channel: <accepted purpose>}`. A slot that
changed between the two calls is 409 `channel_occurrence_changed`, and the
client resolves once more rather than starting stale media.

The runtime switch `library_channels_enabled` is always compiled and affects
resolve/session admission only. Listing, preview, authoring, empty-state help,
and existing schedules stay inspectable while it is off. Its Developer
readiness rows are advisory and cannot veto an administrator's explicit save.

Preview cursors bind the caller, normalized recipe, candidate-content digest,
and offset. Guide cursors bind the caller, channel-generation state, requested
window, and final ordered occurrence. A changed binding is `409
catalogue_changed` instead of a page assembled from two catalogue or schedule
states. Create, update, rebuild, and delete idempotency records live for 24
hours, are capped at 1,000 per account, and reject a reused request identity
with a different normalized operation.

Following authorization is not a one-time check. Every finite-HLS control
exchange reloads the durable session purpose and current channel state. A
disabled or deleted channel ends following with `410 channel_unavailable`; an
unavailable authoritative store refuses the exchange with `503
channel_store_unavailable`. Detached **Watch from start** playback has ordinary
VOD purpose and is unaffected by later channel state.

## 18. Trakt and the Curator seam

| Method | Path | Auth | What it does |
|---|---|---|---|
| GET | `/api/v1/trakt/status` | admin | Link state, sync state, pending device code |
| POST | `/api/v1/trakt/link` | admin | Begins the device-code flow |
| DELETE | `/api/v1/trakt/link` | admin | Deletes the stored token pair |
| POST | `/api/v1/trakt/sync` | admin | Nudges the sync loop; returns status immediately |
| GET | `/api/v1/coming-soon` | bearer | Curator calendar proxy, 28-day horizon, 15-minute cache |
| GET | `/api/v1/monarr/status` | admin | Active probe of Curator, plus watch-outbox counters |

### 18.1 Trakt

`POST /trakt/link` starts an OAuth 2.0 **device-code** flow — no redirect, no
callback URL, no browser round trip through plurx. It returns immediately with
`pending: {user_code, verification_url, expires_in}`; the client shows the
code and polls `GET /trakt/status`. A background task polls Trakt on the
interval Trakt named, adding 2 s on a slow-down, until expiry. On success the
access token is used once to learn the username, and **both tokens are sealed
with the node's credential key before the store write**, so the pair is never
durable in cleartext even momentarily. Missing client id or secret is a 409
telling you to add them first. Only one pending link exists per server, so a
second POST replaces the first.

`status` reports `configured` (both settings are saved — it says nothing about
linking), `linked`, `trakt_username`, `connected_at`, `last_sync_at` (omitted
when never), `syncing` (process-wide), `note`, and `pending`. `pending` is
suppressed when it belongs to another user, or when the link now exists and
the attempt carried no error — so a completed link clears itself while a
failed one stays visible until retried.

`DELETE /trakt/link` deletes the local row and drops in-memory scrobble
sessions. It does **not** call Trakt to revoke the token.

`sync` is a single notify on the loop's kick channel: it does not wait, does
not report the outcome, and cannot fail. The result appears later in `note`
and `last_sync_at`. The loop itself runs hourly, is skipped on a node that is
not a committed voter, refreshes the token when it is within 24 hours of
expiry, and then gates on Trakt's own last-activities value — if nothing moved
remotely, nothing moved locally, and a sync has run before, the pass returns
immediately. Otherwise it is genuinely bidirectional: it pulls watched movies,
watched shows and playback positions in, and pushes local-only watches out in
chunks of 500. Live scrobbling runs separately from the playback handlers.

### 18.2 `GET /api/v1/coming-soon`

Readable by any signed-in user, because it feeds the home screen. It proxies
Curator's calendar over a 28-day horizon with a 10-second timeout.

**The proxy is the feature.** A browser calling Curator directly would need
Curator's API key in its own JavaScript, and that key can edit the whole Curator
library. One server-side hop removes it.

The cache is process-global with a 15-minute TTL, not keyed by user — so each
node in a cluster keeps its own — and **the failure result is cached too**. A
Curator outage therefore produces an empty rail for the full quarter-hour even
after Curator returns.

When Curator is unreachable the fetch error is swallowed and logged, and the
response is still `200 {"configured": true, "entries": []}`. **There is no
distinction on the wire between "monarr is down", "the key was rejected", and
"the calendar is genuinely empty."** Unset settings return
`{"configured": false, "entries": []}` instead — an absent rail rather than an
error, because not pairing Curator is a valid choice.

Each entry is `{date, kind, title, detail, has_file, poster?, item_id?}`.
`poster` is always a plurx URL, and `item_id` is the local item so the card is
clickable. Curator's own ids are used server-side to resolve the entry and are
deliberately **not** serialized: forwarding another application's ids invites
someone to build on them.

Artwork is resolved *after* the cache read, so a title that finished scanning
two minutes ago picks up its poster immediately rather than staying pictureless
for the rest of the quarter-hour. The provider fetch is an SSRF boundary: a
relative path is resolved against TMDB, and an absolute URL is accepted only
over HTTPS on port 443, with no userinfo, and only from `image.tmdb.org`,
`static.tvmaze.com` or `covers.openlibrary.org` — redirects re-checked against
the same list, non-image responses rejected, and a 15 MiB ceiling on both the
declared and the streamed length.

### 18.3 `GET /api/v1/monarr/status`

Deliberately an active probe rather than a cached flag: the person looking at
it has just typed a URL and a key. It reads Curator's status endpoint and
changes nothing.

`{configured, reachable, version?, error?, watched_pending, watched_sent,
watched_failed}`. When `configured` is false it returns without probing, so
`reachable: false` with no `error` means "not configured" while
`reachable: false` with an `error` means the probe ran and failed. The three
error strings separate the three failures that look identical from a settings
screen: `cannot reach Curator at <url>: <root cause>`, `Curator rejected the API
key`, and `Curator returned <status>`. The root cause is the *deepest* error in
the chain, not the HTTP client's outer wrapper — the difference between a DNS
failure and a refused connection is the entire diagnosis in a two-container
setup, and the outer message discards it.

**How to read it, and the trap.** `configured` is an echo of what is stored
and goes true the moment a URL and a key are saved, correct or not.
`reachable` is live *only for the moment you called it*. And
`reachable: true` does **not** imply the coming-soon rail works: this probe
hits Curator's status endpoint, the rail hits its calendar. A key Curator
accepts for one and not the other reads as connected beside a permanently
empty rail, and §18.2 explains why `/coming-soon` will not tell you either.
When status says connected and the rail is empty, the answer is in the log at
target `plurxd::integrate`, message `coming-soon fetch failed` — and remember
the empty result is cached, so a fix takes up to 15 minutes to show. The three
`watched_*` counters are an unrelated fourth answer: `watched_pending` climbing
means Curator is down and the outbox is doing its job, while `watched_failed`
is terminal.

### 18.4 Where the scoped-key wall actually stands

[INTEGRATION.md](INTEGRATION.md) §2 describes a wall that runs both ways, and
it is real — but it stands on `POST /api/v1/scan` and
`GET /api/v1/scan/requests/{id}` (§6.3), not on either route in this section.
`/coming-soon` takes a bearer and `/monarr/status` takes an admin; neither
accepts a scoped key, and a `plx_` key on them is a 401.

That is the direction of the data, not an oversight: scan is Curator calling
**into** plurx and needs a narrow credential Curator can hold, whereas these
two are plurx calling **out** to Curator, holding Curator's key server-side. The
credential that matters here is Curator's key in plurx's settings, and keeping
it off the client is the whole reason both routes exist.

---

## 19. Cluster administration

Every route is admin unless the row says otherwise. `/cluster/status` and
`/cluster/support-bundle` additionally accept the cache-only admin proof
(§2.2) so they still answer when the store cannot be reached.

| Method | Path | Auth | What it does |
|---|---|---|---|
| POST | `/api/v1/cluster/join-tokens` | admin | Mints one single-use **voter** join token |
| POST | `/api/v1/cluster/learner-join-tokens` | admin | The same, wire-distinct, for a **learner** |
| GET | `/api/v1/cluster/nodes` | admin | Live roster, capacity, protocol range, per-node readiness |
| GET | `/api/v1/cluster/status` | admin (cache-only ok) | The aggregate: roster + own snapshot + cached peer observations |
| GET | `/api/v1/cluster/support-bundle` | admin (cache-only ok) | ZIP: the aggregate, a redacted log tail, a README, a manifest |
| POST/DELETE | `/api/v1/cluster/nodes/{node_id}/restart-preparation` | admin | Fences and unfences new mutable media work on **this** process |
| POST | `/api/v1/cluster/nodes/{node_id}/promote` | admin | Promotes a ready learner to voter |
| POST/DELETE | `/api/v1/cluster/nodes/{node_id}/maintenance` | admin | Enters and clears the durable maintenance fence on **this** process |
| POST | `/api/v1/cluster/election` | admin | Forces a leadership change |
| GET | `/api/v1/cluster/ingress` | bearer | Up to 8 reachable peer **origins** to retry a media capability against |
| GET | `/api/v1/cluster/media` | admin | Media-pool placement diagnostics |
| POST | `/api/v1/cluster/media/offers` | admin | Diagnostic placement offers for one file |
| POST | `/api/v1/cluster/leave` | admin **+ body `node_id` = this backend** | Removes this process from membership, then drains and exits |
| POST | `/api/v1/cluster/protocol/learner/{activate,deactivate}` | admin | Narrows or widens the active protocol range |
| DELETE | `/api/v1/cluster/nodes/{node_id}` | admin | Removes another voter or learner — never self |
| POST | `/api/v1/cluster/join/{redeem,finalize}` | **join-token digest** | Stages, then confirms, a voter |
| POST | `/api/v1/cluster/learner/join/{redeem,finalize}` | **learner token digest** | The same on the learner path |
| GET | `/api/v1/cluster/artwork/{filename}` | **cluster HMAC** | Serves node-local artwork to a peer |

`GET /api/v1/cluster/ingress` is the one route here any signed-in user may
call: it returns reachable peer **origins** a client can retry a media
capability against, and never node ids, Raft addresses or private ports.

### 19.1 Joining

```
 admin ─▶ POST /cluster/join-tokens ──▶ {token, expires_at, raft_id}
                    │  token row: issued · role · expires_at
                    │  the operator copies the token to the fresh node,
                    │  which decodes it LOCALLY and never sends it back
                    ▼
        POST /cluster/join/redeem   ── sha256(token) + identity fields
                    │  one Raft transaction: claim origin, reserve token,
                    │  install role intents, publish staged node, CAS the
                    │  active protocol range
                    ▼  token row: redeeming, bound to ONE node id
        POST /cluster/join/finalize ── voter ⇒ committed VOTE
                    │                  learner ⇒ committed member, NOT a voter
                    ▼  token row: redeemed (terminal)
             VOTER ◀── promote ── LEARNER
                    │
     the way out:   maintenance → restart-preparation → leave / DELETE node
```

The issued token carries the cluster id, an allocated Raft id, the bootstrap
peers, and the cluster secrets, sealed with a per-token key. The joining node
decodes it locally and sends only its **digest**, so the secrets never cross
the public listener. These four routes take no account token at all, and the
code states both directions of the reason: requiring one would make a fresh
node impossible to admit, and accepting one would widen an admin token into a
node credential.

Because the route is deliberately unauthenticated, every identity field is
validated *before* any replicated read, and possession is checked with one
indexed lookup before the quorum protocol read — so an unknown digest cannot
buy leader traffic.

**Single use.** The row moves `issued → redeeming → redeemed` and never back.
Redemption binds the credential to exactly one generated node id, so a leaked
token cannot admit a second machine (409 `join_token_reserved`); a retry from
the *same* node id is idempotent. An expired `issued` row is the API's only
**410**. Expiry and redemption both stop reserving the Raft id, so an
abandoned token does not consume the id space.

**The voter/learner split is in the wire, not a flag.** The learner token has
a distinct prefix, distinct associated data and a distinct version constant,
so a build that only knows the voter form refuses it three times over rather
than reinterpreting a learner payload as a voter join. The redeem request has
no role field at all: the role is written into the token row by the issuing
admin and read back from there, because the request body is under the joiner's
control and the token row is under the coordinator's.

### 19.2 `GET /cluster/nodes`

A live read per request. Beyond the obvious roster fields, five distinctions
are load-bearing:

- **`role` vs `is_voter`** — `role` is the durable *admission* role; a voter
  keeps it while Raft temporarily carries it as a non-voting learner during
  catch-up. Conflating them mislabels a joining voter as a permanent learner.
- **`removal_pending`** — a durable removal fence still owns this node, which
  can remain in Raft membership after a rejected or indeterminate request.
  Exposed rather than rendering the node as fully operational.
- **`learner_protocol_ready`** — the binary running *now* proved the protocol
  by writing its capability row inside the same transaction as its current
  heartbeat. `false` here is exactly what blocks activation.
- **`bounded_read_ready`** — a fresh local quorum and apply proof. A learner
  is a read worker only while this is true, and losing it removes the node
  from readiness and placement without changing voter redundancy.
- **`capacity`** — `{voting_nodes, voting_quorum, voting_failure_tolerance,
  non_voting_replicas, ready_read_workers}`. Capacity and quorum are
  deliberately separate arithmetic: a learner adds a recoverable copy and
  possibly a read worker, and increases *neither* quorum nor failure
  tolerance. Do not infer either from the length of `nodes`.

`protocol` reports `active_min`/`active_max` (what the cluster's replicated
features require) beside `binary_min`/`binary_max` (what *this* binary
implements) and `learner_protocol_pending`, which names the nodes blocking
activation.

### 19.3 `GET /cluster/status` — an aggregate that does no fan-out

The request reads three node-owned projections: the 5-second roster cache,
this process's own live snapshot, and the 5-second peer-status cache. It
performs **no** store or peer I/O. The fan-out happens in a background loop
every 3 s, probing at most 8 peers concurrently under one shared 2-second
deadline, each peer capped at 1 s and 256 KiB. An unpopulated roster cache is
503 `cluster_roster_unavailable`; an expired one is `cluster_roster_stale`.

Peer states are kept apart deliberately, because collapsing them prints
"unreachable" for a node that has just replied:

| `observation` | Means |
|---|---|
| `answered` | success, body deserialized, and the `node_id` matched who we asked |
| `unreachable` | silence: not reachable, no HTTP base, or transport said so |
| `timed_out` | too slow inside the per-peer or shared budget |
| `refused` | answered 401/403 — **evidence the node is up**, so look at cluster secrets and clock skew, not at power |
| `http_error` | any other non-success status. Also an answer |
| `invalid_response` | the body would not deserialize, or the sample was older than 5 s |
| `identity_mismatch` | the roster and the answering process disagree about identity — configuration, never transient |
| `unavailable` | *this* node's cache holds nothing for that member |
| `peer_limit` | beyond the 8-peer probe budget, so never probed |

`error_class` narrows `unavailable` further into `not_observed` (the cycle ran
and produced nothing), `cache_unavailable` (no cycle has completed — a node
that just started) and `cache_stale` (cycles have stopped completing).

`sample_age_ms` is computed differently for self and peers on purpose: for a
peer it is the *local monotonic* age of the cache entry, never the peer's own
clock, because skew would otherwise make current evidence look stale or
future-dated evidence look permanently fresh. Anything past 5 s is forced to
`invalid_response` and can never read as healthy.

`verdict` is `{safe_to_restart_one, candidate_node_id, blockers[],
warnings[]}`. The candidate is chosen from committed-roster order rather than
from whoever served the aggregate, so every observer with the same evidence
names the same node — otherwise two operators could concurrently prepare two
different voters. The aggregator never proxies the mutation; it must be sent
to the named node directly.

**How to read it.** Start at `verdict`. `safe_to_restart_one: true` with a
candidate and no blockers is the healthy shape; a `mixed_builds` warning
beside it is expected during a rollout. Then read `nodes[]`: healthy is
`answered`, a small `sample_age_ms`, no `error_class`, and
`status.serving.ready`. When `verdict` blocks, read the blocker codes before
the node rows — `majority_not_preserved`, `leader_term_disagreement` and
`protocol_incompatible` are cluster-wide facts and name no node, while the
rest carry a `node_id` and point you at that node's own status object. And
note that `maintenance[]` is a *separate* per-node judgement: a node unsafe to
put into maintenance may still be a perfectly healthy member, because entering
maintenance has stricter target-specific requirements than surviving a peer's
restart.

### 19.4 Maintenance, restart preparation, promotion, election

**Maintenance and restart preparation must be sent to the target itself** — a
mismatched `node_id` is a 409 saying so. Both are serialized against every
other planned-outage operation, both take a replicated claim, both collect a
*fresh* direct aggregate as a preflight, both open a local fence bounded by
the committed lease, wait for pre-existing admissions to settle, and only then
commit. Any expiry along the way says "run preflight again".

Restart preparation adds two refusals maintenance does not have: the
aggregate's verdict must say a restart is safe, and **this node must be the
aggregate's named candidate** — otherwise it tells you to open the candidate
node directly. Its response carries `restart_commands`, populated only once
the node is drained, and they are **advisory text**: the daemon never invokes
a supervisor.

The maintenance verdict deliberately differs from the rollout verdict: it
admits a *busy* target, because fencing is how work drains, and it admits the
*leader*, because the core hands leadership off before the durable fence
commits.

**Promotion is one-way** — there is no demotion route in this release. It
refuses unless the target is a committed non-voter admitted as a learner, with
a fresh heartbeat showing reachable, bounded-read-ready and zero apply lag,
that then applies a new quorum-confirmed barrier, and with a storage probe no
older than 30 seconds reporting at least 512 MiB of headroom.

**Forced election** exists mainly for a leaderless but otherwise reachable
voter set, so its preflights read *local* applied state; Raft still requires a
real quorum to elect or commit. It refuses without quorum, and while any
removal, promotion, join staging or maintenance is pending.

### 19.5 Leaving, and removing someone else

`POST /cluster/leave` is the irreversible one, and the body's `node_id` must
equal this backend's own — requiring it binds the destructive POST to the
backend the operator confirmed, so a non-sticky load balancer cannot route the
confirmation and the action to different nodes. Membership removal is
**synchronous** and shutdown is signalled only after the change commits, so a
refused leave keeps serving normally. A voter self-leave is permitted even for
the leader, because the departing process is still alive to settle its owned
work and commit a membership excluding itself.

`DELETE /cluster/nodes/{node_id}` refuses self-removal (use leave), the
current leader, and any removal that would leave fewer than three voters.
Offline work owned by the target is settled **before** the membership change
commits, never after, because a half-committed removal leaves packages owned
by a node that no longer exists. It is irreversible: the node must re-join
with a new token.

Once removed, the node's serving role is `Fenced` and every route but
`/healthz` and `/metrics` answers `node_removal_fenced` (§3.2).

### 19.6 `GET /cluster/artwork/{filename}`

The only route authenticated by a filename-bound HMAC. The caller sends its
node id, a timestamp and a signature over
`"plurx-artwork-v1\n{node_id}\n{timestamp_ms}\n{filename}"`, keyed by the
cluster API secret. Verification requires the timestamp within **60 s**, a
valid MAC, **and** that the named node is currently a live member. Anything
else is a bare 401.

The filename is bound *into* the signed message, so a captured proof
authorizes exactly one file and nothing else. It does not accept an account
bearer because this is a peer-to-peer capability, not a household one — the
same rule the join routes state from the other direction. And it **never
proxies a second hop**: the handler reads local bytes only, so a filename
absent everywhere is a bounded 404 instead of a fan-out cycle around the
cluster. The user-facing image route (§6.6) is the one that races peers.

### 19.7 `GET /cluster/support-bundle`

A ZIP of four entries: the aggregate as pretty-printed JSON, the last 200
entries of the **cluster** log ring, a README stating what the archive
excludes, and a SHA-256 manifest. Every log message passes through a redactor
that replaces the whole message when it contains any of `authorization`,
`bearer`, `token`, `secret`, `password`, `signature`, `api_key`, `username`,
the join-token prefix, or any path separator, and truncates whatever survives
to 512 characters. Size is bounded on every axis: 64 roster rows, 8 peer
payloads each capped at 256 KiB when fetched, 200 log entries.

---

## 20. The Plex-compatible façade

Mounted at Plex's own absolute paths, **not** under `/api/v1`. Tier 1: the
endpoint set the Kodi-family clients actually use ([CLIENTS.md](CLIENTS.md)
§3). It is a façade over the *same* services rather than a fork, so a bug
fixed in the decision engine is fixed for a native client and for Kodi at
once.

| Method | Path | Auth | What it does |
|---|---|---|---|
| GET | `/` | none | Capabilities container for Plex clients, the web app for browsers |
| GET | `/identity` | none | `machineIdentifier` and version, before sign-in |
| GET | `/library` | `X-Plex-Token` | Two children: `sections` and `recentlyAdded` |
| GET | `/library/sections` | `X-Plex-Token` | One directory per Movies/Shows library |
| GET | `/library/sections/{id}/all` | `X-Plex-Token` | Up to 5000 top-level items, with per-user view state |
| GET | `/library/metadata/{key}` | `X-Plex-Token` | One video (movie/episode) or directory (show/season) |
| GET | `/library/metadata/{key}/children` | `X-Plex-Token` | Children of a show or season |
| GET | `/library/metadata/{key}/{kind}` | `X-Plex-Token` | Artwork — `art` serves the backdrop, anything else the poster |
| GET | `/library/parts/{file_id}/{mtime}/{name}` | `X-Plex-Token` | Direct play with range support. `{mtime}` and `{name}` are parsed and discarded; only `file_id` selects the file |
| GET | `/photo/:/transcode` | `X-Plex-Token` | Image-resizer shim. Parses `?url=` and returns the original — **no resizing happens** |
| GET | `/:/timeline` | `X-Plex-Token` | Writes playback progress |
| GET | `/:/scrobble` · `/:/unscrobble` | `X-Plex-Token` | Marks watched/unwatched, cascading over the item tree |
| GET | `/search` · `/hubs/search` | `X-Plex-Token` | Identical handler; text search capped at 50 hits |

**Tokens.** `X-Plex-Token` values are ordinary plurx tokens, not plex.tv
tokens. The value is read from the header or from a same-named query
parameter, hashed, and looked up like any other. There is no anonymous
fallback: a missing or unknown token is a 401. Only `/` and `/identity` are
credential-free, so discovery works before sign-in. The access log strips the
entire query string on every request rather than enumerating token spellings.

**Format.** Every façade response is XML with
`Content-Type: application/xml;charset=utf-8`. Content negotiation is not
implemented — see §23.

**How `/` decides.** The dispatch is header-based, not User-Agent-based: any
of `X-Plex-Token`, `X-Plex-Client-Identifier` or `X-Plex-Product`, or an
`Accept` containing `xml` but not `html`, gets the Plex capabilities
container; everything else gets the web app shell. (A separate User-Agent
classifier exists for playback network priors and is not consulted here.)

**The machine identifier** is the node id when the cluster advertises itself
and the replicated instance id otherwise. It must match what GDM advertises as
`Resource-Identifier`, because Plex clients dedupe GDM records on that field.
The advertised version is bare semver on purpose: several clients compare it
numerically and a git build stamp would not parse.

**GDM discovery.** A UDP responder on `239.0.0.250:32414` answers only
payloads beginning `M-SEARCH`, unicast back to the sender, with
`Content-Type: plex/media-server` and the identifier, name, port and version.
It is multicast-TTL-scoped to the LAN, which is what keeps it from being a
reflection amplifier, and it is **disabled entirely when the HTTP bind is
loopback** — publishing a loopback address would give every client a button
that can never connect. The port can be overridden by environment variable; an
unparseable value or a port the kernel refuses disables the responder with a
warning rather than failing startup.

**plex.tv is never contacted.** There is no PIN-link flow and no sign-in
redirect anywhere in the crate. Tier 2 — emulating plex.tv so Infuse, VidHub,
Symfonium and the official Plex apps work — is deferred.

**What the façade does not expose.** Books and Home libraries have no honest
Plex section type, so they are absent from `/library/sections` and every
metadata handler 404s them; the same is true of book, audiobook, folder, video
and photo item kinds.

---

## 21. Internal node-to-node RPCs — not public API

> **Do not build against anything in this section.** These paths are a private
> control plane between plurxd processes in one cluster. They carry no
> compatibility promise, and their paths, bodies and status codes change
> without notice or version bump. **Account credentials are never accepted on
> this surface and never forwarded across it** — a user or admin bearer, an
> `X-Plex-Token`, or an HLS capability gets a 401 at every one of these
> handlers, because they read peer authority out of the cluster headers and
> nothing else. That is asserted by test, not by convention. The paths are
> documented here so that an operator reading an access log or a firewall rule
> can tell what they are looking at.

### 21.1 What signs a request

Per-node **Ed25519** cluster authority — a durable per-node key. No shared
secret and no HMAC is used on any route in this section (the artwork HMAC in
§19.6 is a different wire).

```
   caller node                                receiver node
   ───────────                                ─────────────
   sign_internal_peer_request(               exact_auth_from_headers()
     target, ts, nonce,          ──HTTP──▶   authorize_internal_peer_*(
     METHOD, PATH, BODY)                       auth, METHOD, PATH, BODY)
        Ed25519 over the exact                    │
        route + raw bytes                         ▼
                                              handler
   authorize_internal_peer_response  ◀──────  sign_internal_peer_response(
     (status ‖ body)                            nonce, path, status ‖ body)
```

Headers: `x-plurx-cluster-node`, `-target`, `-time-ms`, `-nonce`,
`-signature`, and `x-plurx-response-signature` on signed responses. Four
predicates gate the routes — one for the activity snapshot (committed voters),
one for exact request authority (any committed member, learners included), a
read variant under a tighter window, and a voter-only variant.

Common checks: the target must be this node; the timestamp must be within
**30 s** (**5 s** for the read variant); sender and target must differ; the
signature must be 128 hex characters over the exact method, path and raw body;
the nonce must be a canonical UUID admitted once, from a bounded replay cache
that *refuses* rather than evicts when full; and a global rate bucket runs
ahead of any signature work. The transport disables redirects, reads under one
deadline and a byte budget, rejects an over-budget `Content-Length` before
streaming, and refuses a response signed for the wrong node or nonce.

### 21.2 The routes

| Method | Path | Body limit | What it does |
|---|---|---|---|
| GET | `/_internal/v1/activity-snapshot` | — | Node-local delivery snapshot |
| GET | `/api/v1/internal/cluster/operations-status` | — | This node's own operations status, for the aggregate |
| POST | `/api/v1/internal/auth/cache-revocation` | 256 B | Propagates one credential-revocation phase |
| GET | `/internal/v1/media/snapshot` | — | This node's media-pool snapshot |
| POST | `/internal/v1/media/offers` | 64 KiB | One placement bid; starts no work |
| POST | `/api/v1/internal/media/shared-cache-canary` | 1 KiB | Proves shared-cache identity and generation |
| GET | `/internal/media/fragment-index/{cache_key}` | — | Streams the verified local fragment index |
| POST | `/_internal/v1/live-tv/snapshot` | 16 KiB | Tuner readiness and lineup for the current generation |
| POST | `/_internal/v1/live-tv/guide` | 16 KiB | The owner's cached programme guide, relayed verbatim. Deliberately not gated on the Live TV protocol capability: an owner that predates the guide answers 404 and the ingress renders "no guide yet" rather than taking Live TV down across a mixed fleet |
| POST | `/_internal/v1/live-tv/start`, `/_internal/v2/live-tv/start`, `/_internal/v1/live-tv/activate` | 16 KiB | Starts and activates a tuner session on the owner; v2 carries the exact signed live playback envelope |
| POST | `/_internal/v1/live-tv/resource` | 16 KiB | Fetches a playlist, segment or status for an owned capability |
| POST | `/_internal/v1/live-tv/stop`, `/_internal/v1/live-tv/drain` | 16 KiB | Releases a capability; drains below a generation |
| POST | `/_internal/v1/live-tv/retire`, `/_internal/v1/live-tv/resume`, `/_internal/v1/live-tv/start-state` | 1 KiB | Retires a viewer's public start id on the owner, hands back the session it still owns, or reports what became of it. Three paths rather than one with a mode flag: `resume` selects a session, cancels the others and fences an id it has never seen, and a status read may do none of that. New paths rather than new fields on the signed start bodies: an owner that predates them answers 404, which an ingress renders as a typed answer that proves nothing about the tuner |
| POST | `/internal/cluster/media/sessions/start`, `/internal/cluster/media/sessions/activate` | 96 / 128 KiB | Starts and confirms a remote media session |
| POST | `/internal/cluster/media/sessions/prepare` | 96 KiB | Validates an already-reserved successor identity, primes its durable recipe on the target owner, and returns only after the existing actor slot accepts it |
| POST | `/internal/cluster/media/sessions/abort`, `/internal/cluster/media/sessions/relay` | 96 KiB | Settles an abort; relays one owned HLS resource |
| POST | `/internal/cluster/media/sessions/control` | 20 KiB | Relays one playback-control exchange |

The five path prefixes are historical, not a versioning scheme. In particular,
the `/api/v1/internal/…` ones are inside the API prefix **by spelling only** —
they are registered on the root router, so no API extractor and no account
auth ever runs on them. `/_internal/v1/live-tv/snapshot` is a POST despite the
name, because it carries a request body.

A non-voting learner serves only a subset of this surface; everything else
returns `learner_route_ineligible` (§3.2).

---

## 22. Web app, PWA and health

| Method | Path | Auth | Notes |
|---|---|---|---|
| GET | `/` | none | The app shell, or the Plex container (§20) |
| GET | `/assets/hls.min.js` | none | `public, max-age=604800` |
| GET | `/assets/{cluster-panel,playback-policy,playback-control,live-tv,library-channels,reader}.js` | none | `no-cache` |
| GET | `/assets/reader.css` | none | `no-cache` |
| GET | `/connect.svg` | none | QR code of the server origin, taken from `?origin=`. Refuses anything that is not a bare `http`/`https` origin, and carries no credential |
| GET | `/manifest.webmanifest` | none | `public, max-age=86400` |
| GET | `/icons/{file}` | none | An allow-list of exactly four PNGs; anything else 404s |
| GET | `/download/plurx-android.apk` | none | The sideloadable APK |
| GET | `/healthz` | none | Liveness |
| GET | `/readyz` | none | Readiness |
| GET | `/metrics` | none | Prometheus text exposition |

Every asset is embedded in the binary — the single-binary, works-offline
promise, no CDN.

The APK route is unauthenticated on purpose: it is the client binary, not user
data, and a TV's downloader or browser cannot attach a bearer token. It
resolves an environment override first, then the data directory; when neither
holds a file the response is 404 `no Android app published` and the web UI
keeps its download link hidden.

**The fallback matters when reading a 404.** Any unmatched path under `/api`
returns `404 {"error":"not found"}`; every *other* unmatched path returns the
app shell HTML with **200**, because the SPA uses hash routing. So a mistyped
Plex path answers with HTML and a success status.

### 22.1 The three operational endpoints

All three sit outside `/api/v1`, and `/healthz` and `/metrics` are the only
two paths that survive a fenced node.

`GET /healthz` returns `ok\n` and **never touches storage**, so it stays 200
on a node whose store is unreachable. That is the point: it is liveness, not
readiness.

`GET /readyz` returns `ready\n`, or 503 with a one-word reason —
`maintenance`, `quorum unavailable`, `store unavailable`, `readiness unknown`.
Maintenance is checked first. On a quorum-managed node the fresh quorum
watermark *is* the recent store proof and **no store ping is issued**: turning
readiness into another multi-second store request is exactly wrong at the
moment an isolated node needs to self-fence promptly. Only a single-node
SQLite deployment, which has no quorum proof, falls through to a ping.

`GET /metrics` is unauthenticated and safe to be so **by construction**: the
handler's state is a projection carrying only process-local counters and
background samples, with no store field at all, so it cannot reach library
data even if a future edit tried. The exposition is counts only —
`plurx_build_info`, `plurx_uptime_seconds`,
`plurx_transcode_sessions_active`, `plurx_scan_total{trigger}`,
`plurx_analysis_markers{kind,provenance,confidence}`, and blocks from the
store, membership, Raft, offline, Live TV, playback-control and blocked-GET
subsystems.

---

## 23. What has no spec, what is not implemented, and what older docs say

**There is no OpenAPI document, and nothing generates one.** Two documents
said otherwise until this file was written, and both were corrected in the
same commit: [ARCHITECTURE.md](ARCHITECTURE.md) §5 claimed the native API was
"OpenAPI-specified from day one", and [CLIENTS.md](CLIENTS.md) §1 claimed the
clients share "the server's OpenAPI-generated types". They do not; the shared
artifact is `tests/contracts/native-api.json`, a wire fixture compiled into
both native clients, and this document.

**The Plex façade does not negotiate JSON.** ARCHITECTURE.md §5 said "XML
`MediaContainer` by default, JSON on `Accept: application/json`". Every façade
handler returns XML unconditionally; `Accept` is read in exactly one place,
the root dispatch in §20.

**Four Plex endpoints ARCHITECTURE.md §5 names are not routed at all**:
`/video/:/transcode/universal/decision`,
`/video/:/transcode/universal/start.m3u8`, `/:/progress`, and `/playlists`.
`/library/recentlyAdded` and `/hubs` are advertised by the containers that
`/library` and `/` return and have no route either. Per §22, all of them
answer with the app shell and a 200 rather than a 404, which is worth knowing
before you conclude a client is misbehaving.

**Two more places where an older document disagrees with the code**, both
resolved in favour of the code here:

- `PLAYBACK.md` states that a transcode is "sdr, always". There is an HDR10
  transcode rung now: a client that proves HEVC Main10 PQ presentation can
  request it, and the server still refuses it for a source, rung or build that
  did not prove the chain (§7).
- `PLAYBACK.md` describes `GET /files/{id}/hls/start` as live wiring. It is a
  deprecated bridge (§9.1).

**Typed error codes are not centrally enumerated.** There is no enum; each
call site passes a literal. The codes documented in this file are the ones
their owning sections emit, plus the five middleware codes in §3.2, which are
reachable everywhere. A mechanical sweep of the crate finds roughly 75
distinct literals and also finds false positives, so it is not a substitute.

**This document is not generated, so it can rot.** Three things keep it
honest, in descending order of reliability: `tests/contracts/native-api.json`
gates the response shapes the native clients depend on;
`tests/operations/test_docs_index.py` fails the build if this file stops being
listed in [README.md](README.md) or if any link here stops resolving; and the
repository's rule that a change to behavior changes the docs in the same
commit. Nothing yet asserts that every route in the router appears in a table
here — that is the obvious next test, and until it exists, treat
`crates/plurxd/src/http/mod.rs` as the authority and this file as its
description.
