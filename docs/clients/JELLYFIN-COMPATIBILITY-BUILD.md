# Jellyfin compatibility — build contract for Infuse and Android TV

**Status:** open · Opus approved; R1–R8 and S1–S4 reconciled; J0 unproved ·
**Written / revised:** 2026-10-02 · **Original source:** `4d7257019` ·
**Review source independently checked:** `f1f1390f1` · **Publication base:**
`9a719fcb7` · [First review](JELLYFIN-COMPATIBILITY-REVIEW.md) ·
[Re-review and disposition](JELLYFIN-COMPATIBILITY-REREVIEW.md)

Companion to [JELLYFIN-EMBY-COMPATIBILITY-OUTLINE.md](JELLYFIN-EMBY-COMPATIBILITY-OUTLINE.md)
(the whole effort), [API.md](../API.md) (existing HTTP contracts) and
[PLAYBACK.md](../PLAYBACK.md) (existing delivery policy). This is the detailed
first-release build and review contract. Work milestone by milestone; retain
the real-client evidence before expanding the API. If compatibility appears
to require fabricated buffer/render observations, weaker authentication, a
second transcode planner or a change to native playback policy, record the
specific conflict and resolve the design before implementing that shortcut.

All proposed names, SQL and interfaces below are **new design**, not existing
APIs. Source links identify integration points; review corrections refer to the
immutable `f1f1390f1` tree, not old line numbers in this checkout. Re-verify
them against the intended build base. No Rust implementation was changed for
this revision; documentation and pre-commit checks are not interoperability
evidence. The docs-only publication branch starts at `9a719fcb7`; unrelated work in
the original checkout is excluded. The re-review records source comparisons
and the remaining evidence boundary.

## 1. Release contract — two named clients, online movies and TV

### 1.1 Clients and evidence boundary

| Client | First platform | Purpose | Version status |
|---|---|---|---|
| Infuse 8.5 or later | Apple TV / tvOS | Living-room direct playback and explicitly selected server transcoding | Pin exact app/OS versions and capture the transcode-selection request in J0 |
| Official Jellyfin for Android TV | Android TV hardware | Independent native playback, DeviceProfile negotiation and server HLS | Pin released app, source revision, OS, device and player backend in J0 |
| Findroid, optional | Android phone/tablet | Additional direct-play-only coverage | Pin only if included; never count it as transcode/remux evidence |

[Firecore's 8.5 announcement](https://community.firecore.com/t/infuse-8-5-smarter-streaming/60410)
introduces user-selected server transcoding. Its exact request flow still
needs capture. [Findroid](https://github.com/jarnedemeulemeester/findroid)
explicitly documents direct play only. The required Android target is the
[native TV client](https://github.com/jellyfin/jellyfin-androidtv), not the
[Android mobile web wrapper](https://github.com/jellyfin/jellyfin-android).
No TV device's availability or codec support is assumed until J0 records it.

**Required viewer flow:** manually connect → sign in → browse a movie or
episode → inspect artwork and tracks → play → seek forward/backward → pause
and resume → stop → resume on the other client → mark watched/unwatched.
The release proves both a forced transcode and a remux in real requesting
clients, using Android TV as the primary profile/HLS reference and measuring
Infuse's explicit transcode option. An API-only request is not player proof.

**Not in this release:** Emby, automatic LAN discovery, Jellyfin web assets,
Quick Connect, server administration, plugins, music/books/home-video/photo
libraries, Live TV/DVR, downloads/offline sync, casting, remote control,
SyncPlay, custom PGS overlays and native-equivalent prepared handoffs.
Findroid's optional download UI must receive an honest refusal from
`/Items/{id}/Download`. Bitmap subtitle burning follows the current
session-grade guard described in §6.3; the legacy consent bit is not authority.

### 1.2 What “supported” means

A supported row identifies app/build, OS, hardware, Jellyfin protocol baseline,
Plurx SHA, media fixtures and passed operations. Do not advertise “all Jellyfin
clients”. Unsupported features are absent or reported honestly, never
successful no-ops for mutations. A client upgrade does not inherit acceptance
automatically; rerun its smoke flow and affected detailed cases.

## 2. Existing services — reuse the owners that already exist

| Responsibility | Existing source | Build constraint |
|---|---|---|
| Public router | [http/mod.rs](../../crates/plurxd/src/http/mod.rs) | Add a distinct façade; preserve native and Plex route behavior |
| Prior compatibility pattern | [http/plex.rs](../../crates/plurxd/src/http/plex.rs); pure crate [map.rs](../../crates/plurx-compat-plex/src/map.rs), [xml.rs](../../crates/plurx-compat-plex/src/xml.rs), [gdm.rs](../../crates/plurx-compat-plex/src/gdm.rs) | Reuse layering; Plex timeline is not the complete watch-service precedent |
| Login capacity, throttling and token issuance | [http/auth.rs](../../crates/plurxd/src/http/auth.rs) | Extract the currently inline handler logic and private helpers; preserve per-node throttle, address handling and Argon2 bounds |
| Token expiry and revocation integration | [Store](../../crates/plurx-core/src/store/mod.rs) and [http/extract.rs](../../crates/plurxd/src/http/extract.rs) | Both Store lookup methods honor expiry; share AuthUser semantics for typed expiry and cache-proof bookkeeping |
| Bounded catalogue reads | [store/mod.rs](../../crates/plurx-core/src/store/mod.rs), `CatalogueReader`; [http/browse.rs](../../crates/plurxd/src/http/browse.rs) | Preserve bounded reads; batch file, artwork and watch projections |
| Domain entities | [domain.rs](../../crates/plurx-core/src/domain.rs) | Project existing items/files/users; no second catalog |
| Watch state and effects | [http/watch.rs](../../crates/plurxd/src/http/watch.rs), [progress.rs](../../crates/plurxd/src/progress.rs) | Share coalescing, watched transitions, activity and telemetry semantics |
| Capability model | [playback/caps.rs](../../crates/plurx-core/src/playback/caps.rs) | Missing capability is not permission; do not flatten conditional profiles into an unsafe union |
| Decision, direct and progressive paths | [http/stream.rs](../../crates/plurxd/src/http/stream.rs) | Reuse decisions, source validation, existing byte-range semantics and stream resource accounting |
| HLS create/release/control | [hls/create.rs](../../crates/plurxd/src/http/hls/create.rs), [hls/release.rs](../../crates/plurxd/src/http/hls/release.rs), [hls/control.rs](../../crates/plurxd/src/http/hls/control.rs) | Share application services; do not make loopback HTTP calls |
| Cluster routing and release | [media_sessions.rs](../../crates/plurxd/src/media_sessions.rs) | One owner and existing epochs, deadlines, relay and cleanup |
| Native playback control | [playback_control.rs](../../crates/plurxd/src/playback_control.rs) | New external observations must remain distinguishable from native evidence |
| Developer card | [settings-developer.js](../../crates/plurxd/src/web/pages/settings-developer.js) | Explicit switch, advisory readiness and graduation text |

Two current signatures show where translation meets an existing owner:

```rust
// Existing: crates/plurxd/src/http/auth.rs; adapter must preserve this service's
// admission and identity semantics if the Axum handler is factored out.
pub async fn login(
    State(state): State<AppState>,
    ClientPeer(peer): ClientPeer,
    headers: HeaderMap,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, ApiError>

// Existing: crates/plurxd/src/progress.rs. This is coalesced progress, not a
// durable per-event acknowledgement and not the whole watch-handler service.
pub async fn put(
    self: &Arc<Self>,
    user_id: i64,
    item_id: i64,
    position_ms: i64,
    duration_ms: Option<i64>,
) -> Result<ProgressUpdate, StoreError>
```

Do not call only `ProgressCoalescer::put` and omit the other effects in
`http/watch.rs`. Extract the smallest shared application operation with a
regression proving native behavior is unchanged. The same rule applies to
auth and session creation: an internal service extraction is acceptable;
copying a large handler into the façade is not.

## 3. Architecture and proposed source ownership

```text
 client HTTP / optional observed WebSocket traffic
                  |
     /jellyfin route adapter + credential parser
                  |
       typed DTOs / IDs / query / profile translation
                  |
          authenticated compatibility service
          |                |                 |
     catalogue/watch   playback negotiation   session binding
          |                |                 |
       Store         native media planner    coordinator/owner
                                              |
                                    direct/remux/HLS workers
```

Proposed new source groups, to be created during implementation:

| Group | Proposed location | Job |
|---|---|---|
| Pure protocol layer | `crates/plurx-compat-jellyfin/src/` | DTOs, tick/ID wire forms, query parsing, profile predicates and golden fixtures; no FFmpeg or HTTP client |
| HTTP adapter | `crates/plurxd/src/http/jellyfin.rs` plus `http/jellyfin/` children | Keep a zero-argument `router()` in the flat entry file for route inventory; children hold handlers |
| Application bridge | `crates/plurxd/src/jellyfin/` | Negotiation, external lifecycle and mapping to existing services |
| Durable state | Additions under `crates/plurx-core/src/store/` | ID allocation, device/session binding and transactional activation; both supported Store backends |
| Contract evidence | `tests/compat/jellyfin/` | Sanitized protocol traces, trace manifest and replay/acceptance support |

These are proposed paths, not links to files that already exist. Avoid one
large handler file. Keep Jellyfin-specific DTOs out of the native API and
keep protocol spelling out of the core media planner. Add the new crate to
workspace validation scope; verify new tests really run in the selected lane.

### 3.1 Public address and routing

Initial connection URL: `https://<public-host>/jellyfin`. Use Plurx's existing
port; a separate port or listener is not required by this design. Do not
replace `/`, which already serves the Plurx UI and Plex requests.

J0 must prove both apps retain that base path for login, artwork, streaming
and subtitle requests. Add root aliases only for an observed client need and
after checking every collision. A reverse-proxy subdomain can be a later
deployment choice; it must not become a hidden prerequisite.

Use client-base-relative URLs. Plurx has no configured public-origin setting;
`trusted_proxies` resolves client IP, not a public URL. Do not invent that
setting or construct origins from request `Host`. J0 must prove the exact
leading-slash/join behavior for each client and resource type.

Alias native HLS capability paths under `/jellyfin`, including master/media
playlists, initialization fragments, segments and subtitle resources. Rewrite
the native root-absolute `playlist_url` to the tested façade form; relative
inner URIs then stay in that alias. Preserve capability semantics and redact
these paths in `safe_trace_target`, including the tested case variants. No
internal node addresses or origin-changing redirects may leak into responses.
If a target truly requires absolute URLs, add and review a public-origin
contract after J0 proves it necessary.

Always register the `/jellyfin` nest, even when disabled, with its own JSON
404 response for disabled and unknown routes. Otherwise the native SPA
fallback returns 200 HTML and looks like a broken Jellyfin server. Test both
the bare mount and subpaths through TLS termination and a reverse proxy.

## 4. Protocol baseline — traces choose the subset

### 4.1 Freeze a reference before implementing compatibility

J0 records an exact released Jellyfin server version and immutable container
digest, its OpenAPI SHA-256, exact client versions, Android TV source revision,
player backend, fixture hashes and observed requests. Use a disposable
reference server with synthetic media and a dedicated test account.

The official [API entry point](https://api.jellyfin.org/) returned HTTP 403
during this document's preparation. The author inspected official controller
source and SDK documentation instead. The upstream links in §13 track moving
branches and are research pointers, **not the pinned implementation baseline**.
No specific Jellyfin release number is claimed as supported yet.

Capture method, path, query, relevant headers, request/response schema and
event order for the full §1 flow. Replace credentials and identifying data
with fixtures; preserve repeated-ID relationships and timing deltas. Keep
raw captures outside the repo. Retain only minimized regression fixtures,
their provenance and a manifest identifying which client needs each route.
The manifest must include a **version-implied calls** column: advertised
server version selects client behavior, not just a display label. In optional
Findroid coverage, 10.9 enables trickplay and 10.10 enables media segments.
Trace those requests and provide semantically valid empty responses where
supported; a 404 is not automatically equivalent to an empty list.

For each client's image, subtitle, direct-media and playlist/segment GET,
record whether it sends a query token, header token, or neither. Also capture
Infuse 8.5+ transcode selection, Android TV HLS/remux negotiation, separate unindexed copy and unindexed transcode requests, predictable source
refusals, transient startup retries and a VOD pause longer than 300 seconds.
Capture track/quality renegotiation that omits Stopped for the old play.

### 4.2 Candidate route inventory

Paths below are relative to `/jellyfin`. They are a **candidate inventory**,
not a claim that every current or historical variant is required. J0 resolves
modern versus legacy aliases and records required fields/statuses from the
pinned reference and target-client traces. Observed mandatory routes cannot
be dismissed because they are absent from this initial list.

| Group | Candidate endpoints | Required meaning / limit |
|---|---|---|
| Server identity | `GET /System/Info/Public`, authenticated `GET /System/Info` if requested | Stable logical server identity, public address and tested compatibility version; no admin details in public response |
| Sign-in | `GET /Users/Public`, `POST /Users/AuthenticateByName` | Manual login; always an empty public list; no native enumeration policy exists |
| Current user | `GET /Users/Me`, `GET /Users/{userId}`, `POST /Sessions/Logout` | Own-user data; genuine revocation on logout |
| Libraries/views | `GET /UserViews`, legacy `GET /Users/{userId}/Views` | Supported movie/TV libraries; native users currently share library visibility |
| Items/detail | `GET /Items`, `GET /Items/{itemId}`, legacy user-scoped item routes | Hierarchy, requested fields, stable IDs, actual paging and sorting |
| Home/resume | `GET /Items/Latest`, `/Items/Resume`, legacy user-scoped forms | Correct date order, resumable state and collection shape |
| Television | `GET /Shows/{seriesId}/Seasons`, `/Shows/{seriesId}/Episodes`, `/Shows/NextUp` | Show/season linkage and next episode semantics |
| Search | Item search parameters; `/Search/Hints` if observed | Same supported catalog for authenticated users, deterministic limits |
| Artwork | `GET /Items/{itemId}/Images/{type}` and indexed form if observed | Primary/backdrop mapping, cache tags, bounds and §5.1 image policy; trace token presence |
| Capabilities | `POST /Sessions/Capabilities`, `/Sessions/Capabilities/Full` if observed | Retain this login/device's claims; do not advertise remote control |
| Negotiation | `GET`/`POST /Items/{itemId}/PlaybackInfo` | Media sources, streams, allowed delivery and one play identity; no encoder allocation for browsing |
| Direct media | `GET`/`HEAD /Videos/{itemId}/stream` and extension variants used by targets | Source mapping, existing Range semantics and cancellation; validators are separate work |
| HLS entry | `/Videos/{itemId}/master.m3u8` or exact returned URL | Lazy, idempotent activation followed by existing HLS delivery |
| Subtitle resources | `/Videos/{itemId}/{mediaSourceId}/Subtitles/{index}/Stream.{format}` or exact returned URL | Supported format, selected source/stream and correct media timeline |
| Playback events | `POST /Sessions/Playing`, `/Sessions/Playing/Progress`, `/Sessions/Playing/Stopped`; `/Sessions/Playing/Ping` if observed | Identity-bound start/progress/stop; a ping is not render evidence |
| Transcode stop | `DELETE /Videos/ActiveEncodings` if observed | Only this authenticated device/play's encoding; never a global kill |
| Watched state | `POST`/`DELETE /UserPlayedItems/{itemId}`, legacy `/Users/{userId}/PlayedItems/{itemId}` | Existing watched/unwatched and tree semantics |
| Bootstrap extras | Localization/configuration/display-preference reads, `/Sessions`, WebSocket `/socket` only if required | Implement actual minimum semantics; no unrestricted admin API or pretend mutation success |

For every required endpoint record client/version-implied calls, request limits,
successful status/body,
failure body, null/absent handling, auth, side effects, owner and test. Common
HTTP spelling is PascalCase JSON and client-specific header/query carriers.
Do not infer case behavior, alias behavior or error enums from Plurx's API.

Unknown optional DTO fields may be ignored. Unknown playback constraints
must never grant capability. Unknown mutating routes return an honest error.
A bootstrap route may return an empty collection only where the reference
contract genuinely permits it and the target flow works with that result.

## 5. Identity, authentication and catalog projection

### 5.1 Credentials retain native policy, with explicit façade differences

Parse the target clients' actual `Authorization: MediaBrowser ...`,
`X-Emby-Authorization`, `X-Emby-Token` and `api_key` carriers as proved in J0.
Jellyfin's `api_key` query parameter carries a user access token; Plurx's
native `?token=` has different spelling. Reject `plx_` scoped API keys on
this user façade. Device/client/version fields are not credentials. Accept
the same secret in multiple carriers; reject conflicting secrets. Bound
header parsing and do not globally change native bearer parsing.

`Store::user_for_token` wraps `Store::authenticate_token` and already honors
expiry/revocation. Share the additional `AuthUser` behavior: typed
`session_expired` responses and cache-only admin-proof bookkeeping. Login
requires a real extraction from the Axum handler; throttling, capacity,
password work and IP helpers are currently module-private. Preserve the
existing per-node throttle rather than claiming a cluster-wide login budget.
Token revocation still has cluster-wide semantics.

**Proposed logout ruling:** invalidate only the presented façade token and
its compatibility plays, using the same cluster cache-revocation exclusion.
Do not call native logout wholesale: it also revokes all the user's file
grants, disrupting readers on other devices. Factor token-only revocation
as a shared primitive while leaving native logout's existing scope intact.

**Repeated login:** replace the prior compatibility token for the same
`(user, DeviceId, client family)` after successful authentication, with
transactional replacement and cluster proof invalidation. Never revoke a
native token or another user's token by a client-supplied DeviceId. J1 tests
simultaneous logins, password changes and failed replacement; no plaintext
secret is retained. J0 verifies target behavior before freezing this policy.

User IDs in route/query/body must match the authenticated user; this façade
has no administrative impersonation. Native Plurx currently has no per-user
library ACLs: authentication admits all supported libraries. Do not claim
library-level authorization exists. Still validate item/source/track
membership and prevent cross-user watch or session mutation.

**Proposed artwork ruling, finalized from J0 traces:** allow anonymous
GET/HEAD for mapped movie/TV item artwork only when needed for target-client
parity. Upstream item image reads do not require authentication, and clients
may construct the URL without a token. An unguessable UUID reduces enumeration
but is not access control: anyone with the URL can read that poster. Disclose
this consequence on the opt-in setting and in operations/security docs.
Allow only mapped supported items, no user avatars, listing endpoint,
arbitrary filename/path or unsupported libraries. Quantize requested widths
up to the next width in the existing 300/500/780 derivative set (default 500),
clamping above 780; no arbitrary size or format combinations. Anonymous
requests serve existing derivatives. An admitted miss returns 404 and enqueues
deduplicated `(item, width)` materialization through the existing artwork
worker, peer-fetch path, locks and admission. Fetch/resize work never runs
inline in the anonymous request. Coalesce repeated demands, bound queued
keys and retries, and revalidate the current artwork identity before publishing
so a changed poster cannot reuse stale bytes. Do not create detached tasks or
a second worker pool. Requests rejected by the miss budget enqueue no work.
Proposed
miss budget: 20/minute per resolved client address, with a bounded 4,096-entry
60-second idle TTL table; when full, refuse new addresses until entries expire
rather than evicting live budgets. Return 429
when admission is exhausted. Reuse native trusted-proxy address resolution.
The per-address budget bounds request and demand admission; shared worker
admission bounds fetch/resize work. J2 measures first-sync art coverage on an
ingress with neither local source artwork nor warm derivatives, then checks
coverage after materialization and the client's next fetch/sync. Return no
long-lived negative cache entry for a cold 404. Do not assume a client retries
it: if J0 shows it does not, amend the policy to warm the three sizes through
the same bounded worker when the switch is enabled, before qualifying first
sync. Also test width floods, deduplication and queue/admission saturation.
Require authentication if both target clients reliably provide it. J0 records
the selected policy explicitly before J2; do not silently weaken native image
routes. Streams and subtitles remain authenticated at entry, or use the
existing unguessable media capability issued after authenticated negotiation.
A bare item ID or `PlaySessionId` is never a media grant.

Redact all token carriers and media capabilities from logs, traces and metrics,
including failures. Revocation must deny new authenticated media activation;
existing admitted capabilities follow the documented native retirement rules.

### 5.2 Stable IDs across restarts, nodes and row-ID reuse

Use lowercase 32-hex UUIDs; accept dashed spelling where the pinned protocol
permits it. Derive server ID from replicated `instance.id` with dashes removed,
never the node ID. Plex's node-specific advertisement is a different contract.
Catalog item IDs, file/MediaSource IDs and probe stream indices remain distinct.
Do not use DTO array positions as stream indices.

Native entity tables use `INTEGER PRIMARY KEY` without AUTOINCREMENT, so a
removed highest row ID can be reused. Files also retain IDs on path upsert.
A permanent `(kind, native_id)` map or deterministic hash of that pair is
therefore insufficient. Proposed Store-managed schema:

```sql
-- Proposed; integrate with both existing Store migration mechanisms.
CREATE TABLE jellyfin_entity_ids (
    wire_id TEXT PRIMARY KEY,
    entity_kind TEXT NOT NULL
        CHECK (entity_kind IN ('user', 'library', 'item', 'file')),
    native_id INTEGER NOT NULL,
    incarnation TEXT NOT NULL,
    retired INTEGER NOT NULL DEFAULT 0 CHECK (retired IN (0, 1)),
    UNIQUE (entity_kind, native_id, incarnation)
);
CREATE UNIQUE INDEX jellyfin_entity_live
    ON jellyfin_entity_ids(entity_kind, native_id)
    WHERE retired = 0;

-- Repeat with the matching entity_kind on users, libraries and files.
CREATE TRIGGER jellyfin_retire_item AFTER DELETE ON items
BEGIN
    UPDATE jellyfin_entity_ids SET retired = 1
    WHERE entity_kind = 'item' AND native_id = OLD.id AND retired = 0;
END;
```

Allocate a random UUID and incarnation transactionally, returning the winner
under concurrent allocation. Batch page allocations; repeated syncs must not
write new IDs. Install deterministic `AFTER DELETE` triggers on `users`,
`libraries`, `items` and `files` in both backends. FK cascades bypass application
call sites; triggers retire each affected mapping inside the deleting
transaction, before row-ID reuse. Do not use replica-local clocks such as
`unixepoch()` in a trigger. Any later timestamp is advisory and supplied as a
replicated statement parameter; `retired` is the authority. Lazy existence
checks cannot detect delete-and-reinsert of the same row ID. Retired wire IDs
never resolve to a replacement entity. Retain the retired identity/tombstone
or an equivalent permanent non-reuse proof; do not blindly TTL these mappings.
Use [dv_conversion.rs](../../crates/plurx-core/src/store/dv_conversion.rs)'s
`dv_queue_admission_settings_ai` as the precedent for a trigger executing
inside a hiqlite Raft entry. Explicit hiqlite file/item deletes also fire
retirement triggers, independently of FK enforcement. Preserve that coverage:
J1 adds a source guard against `INSERT OR REPLACE` and `REPLACE INTO` targeting
`users`, `libraries`, `items` or `files`, including multiline/quoted SQL and
migration paths. REPLACE can delete without firing DELETE triggers when
`recursive_triggers` is off. Prove the guard catches an injected replacement;
any future exception needs an explicit retirement contract and both-backend
tests, not a silent allowlist addition.

J1 enumerates deletion paths and deletes a library with nested descendants,
asserting every item/file mapping is retired on both backends. Also verify
trigger installation, migration replay and concurrent allocation/deletion.

A same-file path upsert preserves identity, but source fingerprint changes
invalidate its old negotiated recipes. Multiple files remain distinct media
sources. Test delete/reinsert at the maximum ID, cascades, rescan/upsert,
concurrent first sync, restart and alternate-node lookup.

### 5.3 DTO and query contract

Map library → `CollectionFolder`, show → `Series`, season → `Season`, movie →
`Movie`, episode → `Episode`, with parent/series/season IDs preserved. The
wire fields required by the clients include names, item type, runtime,
overview/year, artwork tags, stream facts and per-user playback state.
Return actual measured facts; omit unknown optional metadata rather than
invent codec profiles, play counts or paths. Never expose filesystem paths
as playable URLs. J0 must decide whether any app requires a harmless path
display field distinct from the actual stream URL.

`1 ms = 10,000 ticks`. Convert with checked integer arithmetic; reject
negative or overflowing inputs before storage or seek conversion. Distinguish
missing position from zero, and preserve zero-valued subtitle/audio indices.
Clamp positions to a known authoritative runtime only under the same native
watch/seek policy; do not trust a client to inflate duration or completion.

Supported queries need explicit filters, `ParentId`, recursion, item types,
field projection, sort order, `StartIndex`, `Limit` and total-count semantics.
Filter and order before slicing. Use a stable ID tie-breaker; a static catalog
must return each item exactly once across pages. Exact counts describe the
filtered catalog; offset paging is not a snapshot guarantee during scans.

Initial resource defaults proposed for J0 measurement: 100 items by default,
1,000 maximum per page, 256 KiB negotiation body, 64 KiB event body and the
existing 8 KiB login body limit. Apply limits before expensive work; reject
oversized bodies. Explicit page limits above the cap must be tested against
Infuse's sync loop so clamping cannot silently truncate its library. Batch
watch/file reads and retain the existing catalogue's bounded-read behavior.

External clients do not echo Plurx's commit-index header. The existing
`CatalogueReader::watch_map(user, ids, None)` already uses Authority when
there is no `read_after` fence. Use that path, batched per page, for UserData;
do not build a new session-carried floor. Verify an acknowledged manual change
is visible when the next read reaches another node. Hiqlite token auth is
also a consistent read on each request: measure first-sync artwork and range
storms, including auth cost, instead of adding an unreviewed auth cache.

## 6. Playback negotiation — preserve constraints and ownership

### 6.1 Profile translation is more than codec-name mapping

Jellyfin sends direct-play, transcoding, codec/container conditions and subtitle
profiles. Preserve relationships between codec, container, profile, level,
resolution, frame rate, bitrate, channel count and delivery protocol. Taking
the union of codecs across mutually exclusive profiles grants combinations
the client never accepted.

Proposed translation result:

```rust
// Proposed internal contract, not an existing declaration.
struct CompatibilityPlaybackPlan {
    source_id: i64,
    source_fingerprint: String,
    client_constraints_digest: String,
    allowed_deliveries: Vec<ValidatedDelivery>,
    selection: CompatibilitySelection,
    start_position_ms: i64,
}
```

The native `decide()` returns one decision, not an iterable candidate list.
`DeviceCaps` has flat audio/container/transport lists. Implement an explicit
bounded candidate order in the protocol bridge:

1. Match Jellyfin DirectPlayProfiles/CodecProfiles to the probed source in the
   pure crate, then require the existing native planner's source validation
   and direct-play policy to permit it as well. Protocol matching cannot
   override native source, HDR or transport safety.
2. For a non-direct delivery, select **one** ordered TranscodingProfile and
   evaluate its conditions. Build DeviceCaps from that single container/codec
   tuple. Ask the existing planner and remux/transcode services for a legal
   outcome, honoring the profile's copy flags and transport.
3. Validate the actual output against that exact tuple and its remaining
   predicates. On mismatch refuse, or explicitly try the next profile under
   a bounded deterministic loop. There is no implicit native “next candidate”.
   Unknown mandatory predicates disqualify a candidate, never vanish.

Honor `EnableDirectPlay`, `EnableDirectStream`, `EnableTranscoding`, selected
source/tracks and bitrate limits. Confirm the pinned reference's query-over-
body precedence. A missing profile may use this login/device's registration,
not another user's profile or a guess from an app name. Do not union profile
alternatives or build a second FFmpeg planner. Missing HDR/DV/sample-entry
claims do not grant capability. Log the normalized refusal or selection reason.

### 6.2 PlaybackInfo allocates identity, not a producer

Return truthful `MediaSources`, `MediaStreams`, direct/stream/transcode flags,
subtitle delivery information and `PlaySessionId` matching the reference DTOs.
The first media request activates a selected delivery; `HEAD`, library scans
and repeated negotiation must not spawn encoders. Negotiation can allocate a
bounded, expiring play binding but cannot reserve long-lived transcode scratch.

An HLS entry request may be a GET because that is the external protocol.
Derive native `playback_id` from a domain-separated digest of authenticated
`(user, DeviceId, client family)`: a stable player instance, matching token
replacement scope. It survives renegotiation and token replacement without
allowing old tokens to activate. Derive `request_id` from `PlaySessionId` plus
the normalized delivery digest. A new negotiation/selection then supersedes
the prior producer under the same player identity. Build
`CreateSession` deterministically so retries have the same intent fingerprint.
The existing 60-second durable request claim, response replay, mismatch 409
and released 410 are the mechanism; do not build a second activation ledger. Do not call
the deprecated native HLS start bridge, which synthesizes a user/file key and
can collide between devices. Retries that time out resolve the same claim;
they cannot create a fresh producer just because the HTTP connection ended.

Producer concurrency is one per player instance. J0 must prove that targets
supply a stable DeviceId; missing/ambiguous identity cannot fall back to a
global user/file key. Multiple windows sharing that identity need a traced
stable player discriminator before independent playback can be supported.
Other devices and users always have distinct player scopes. Unactivated
negotiation does not terminate playback; successor activation supersedes it.
Fence the old compatibility binding so its late activation, stop or progress
cannot supersede or mutate the newer play.

Normalize a delivery key from play identity, source fingerprint, validated
selection, start position, transport and the server-owned VOD-only policy.
Resolve any replay after the native claim window through the existing route
and the compatibility terminal tombstone; expiry of a claim must not revive
a released play. Test retries on both sides of the 60-second boundary. Identical requests reuse the result.
A changed seek or selection is a successor attempt, never mutation of the
bytes behind an already advertised segment URL. Reject mismatched item/source
IDs and attempts to change a validated plan by editing URL parameters.
J0 must measure client-generated stream URLs as well as server-returned ones;
support their actual flow without assuming every app uses `TranscodingUrl`.

### 6.3 Direct, remux, subtitles and time origins

Direct streaming shares source validation, range handling, disconnect cleanup
and accounting. At the reviewed tree it does not publish a strong ETag or
304 contract: `If-Range` causes a full response, and a valid multi-range
request selects its first satisfiable member rather than multipart output.
Prove those exact semantics along with `206`, `416`, open-ended/suffix ranges
and HEAD. New validators/conditional caching are separate work only if J0
shows a target requires them. Parallel ranges must not allocate transcodes;
content type and container must describe delivered bytes.

HLS and progressive remux reuse existing media-origin logic. Some clients
report source-relative positions after `StartTimeTicks`; others may report
relative to the delivered stream. J0 pins the behavior for each selected
transport. Translate once at the boundary and keep both source position and
delivery origin in the binding. Test nonzero resume followed by backward seek
and subtitle selection; adding the origin twice is a release-blocking defect.

The existing external text-subtitle endpoint produces WebVTT only and returns
500 on extraction failure. J0 must establish whether targets accept VTT or
request SRT/ASS; support additional conversion only as named work, otherwise
negotiate VTT or refuse. Expose only formats actually implemented. Preserve forced/default/language flags, negative off sentinels
and original stream indices. Validate track membership in the selected file.
Use the existing subtitle pipeline and cache; a refused extraction must not
become a successful empty subtitle response. Verify subtitle URL auth and
timestamp alignment after resume and seek.

Foreign clients cannot consume the Plurx PGS overlay protocol. Offer native
bitmap delivery only when the client can consume it through the chosen
transport. Otherwise use an existing permitted burn route or refuse. In the reviewed
main tree, `subtitle_burn_sdr` remains wire-compatible but is no longer
load-bearing. The real guard is `burn_would_discard_this_session_hdr`, which
returns 422 `hdr_subtitle_burn_refused` when burning would discard the
session's HDR. Reuse that grade-based guard; do not invent a consent rule or
bypass it for a foreign app. This correction is source-version-sensitive:
verify it when porting the docs/implementation onto main.

## 7. Sessions and external observations — the critical design seam

### 7.1 Keep the identities separate

| Identity | Scope | Must not be confused with |
|---|---|---|
| Login/access token | Authenticated user session | Device ID, which is client-supplied |
| Device/session registration | This login plus device/client identity | Another device on the same account |
| Player instance / native `playback_id` | Stable authenticated user + DeviceId + client family | A new identity on each negotiation |
| `PlaySessionId` | One logical playback negotiation/play; part of native `request_id` | Stable player identity, native HLS capability or item ID |
| Delivery attempt | One source/selection/origin under that play | A fresh logical play on every segment fetch |
| Native session/generation/epoch | Existing media ownership and mutation fences | A client-provided sequence or trusted user claim |

Proposed durable binding fields: play UUID, user ID, token digest, bounded
device identity, item/file IDs, source/profile fingerprints, selection,
source origin, state, expiry and current attempt/session reference. Store no
plaintext token. Multiple bindings per player are negotiation identities and
retained terminal records, not permission for concurrent producers. Keep one
active producer per player, with other devices independent. Resolve individual
bindings by play UUID; never collapse their tombstones into a user/item key.

Keep only compatibility identity, its native activation reference, manual-
edit revision and terminal tombstone in the new binding. Reuse existing
`MediaSessionRoute` for owner, epoch and native terminal state, and existing
request claims for activation. Do not duplicate their ownership or renewal
fields. Any ingress can resolve the mapping and delegate to the coordinator.
High-rate observations remain owner-local; no per-segment Raft writes. J1
specifies binding expiry/cleanup and what survives ownership transfer.

Proposed unactivated binding limits: 10-minute TTL, 64 pending bindings per
login/device and 4,096 server-wide. Evict only expired/unactivated bindings;
never evict active playback to satisfy a negotiation flood. Repeated catalog
requests must not leave permanent session rows. J0 may amend these values
with measured client behavior; record the amendment and reason.

### 7.2 Use passive VOD; exclude rolling recovery before allocation

At reviewed main `f1f1390f1`, library VOD production is driven by segment
requests through [prodsched.rs](../../crates/plurxd/src/prodsched.rs), with a
180-second ahead horizon. [vodserve.rs](../../crates/plurxd/src/vodserve.rs)
uses a 300-second idle TTL; successful delivery updates the touch clock in
[vod/serve/delivery.rs](../../crates/plurxd/src/vod/serve/delivery.rs). Native
control is optional on that reader path. Reuse passive delivery and do not
synthesize `ControlRequestV1`. **J0 measured an owner-integration gap:** after
a 343.445-second physical pause the reader was idle-reaped, the owner cleanup
ended its durable route, and the next real request returned
`410 media_session_ended`. The reader-only resurrection regression is
insufficient. [ADR-J0-1](JELLYFIN-PASSIVE-VOD-ROUTE-LIFETIME.md) proposes a
bounded, server-owned passive route grant separate from producer lifetime.
Its service/worker policy and integrated/physical recovery proof must settle
before J4; the claim that no additional native lifetime seam is needed is
withdrawn.

**The rolling fallback is different.** The native
[create manager](../../crates/plurxd/src/transcode/manager/create.rs) can turn
an unavailable VOD prerequisite into rolling live-recovery HLS. Its startup
clock arms at first video playlist and expires after about 30 seconds unless
control reports presentation. A passive client cannot satisfy that contract.
Do not interpret media GETs as rendered frames to avoid this expiry.

**Chosen build policy:** the façade never enters rolling fallback. Add a
server-owned, per-create VOD-only policy at the service boundary, propagate it
through any worker/relay envelope and bind it into request identity. Refuse
fallback **before** allocation; do not create a rolling producer and reject
it afterward, and do not disable recovery globally for native clients.
Resolve predictable prerequisites during `PlaybackInfo`, before advertising a
playable URL. Use known probe duration, index readiness and recipe availability
to evaluate the bounded candidate order in §6.1. Revalidate at activation for
source changes and capacity races; negotiation is not a reservation.

| Native refusal / prerequisite | Compatibility decision |
|---|---|
| `vod_index_pending`: copy without a usable fragment index | Enqueue/promote existing preparation, then immediately try profile-permitted progressive remux, otherwise encoded VOD if `EnableTranscoding`; otherwise refuse negotiation. Do not wait for an index. |
| `vod_source_unsupported`: parameter sets vary | Try a permitted encoded VOD recipe, which does not copy those parameter sets; otherwise refuse. |
| `vod_source_unsupported`: no positive probed duration | Encoded VOD also refuses: it needs a closed film-time plan. Use another independently validated delivery, such as permitted progressive remux, only if its native path supports the source; otherwise refuse negotiation. |
| `vod_transcode_unavailable` / `vod_subtitle_burn_unavailable` | No valid encoded recipe: refuse that candidate, never advertise a URL known to fail. Any other candidate must independently pass profile and native policy checks. |
| Other predictable typed VOD refusals, including `vod_video_geometry_unknown` / `vod_frame_cadence_unknown` (native 422) and `vod_source_rescan_required` (native 409) | Refuse that candidate, map to the pinned protocol error DTO/status, and never advertise its URL. Try another candidate only when it independently passes validation; unknown refusal codes do not grant playback or enter the retry allowlist. |

The index guard in
[vod/serve/create.rs](../../crates/plurxd/src/vod/serve/create.rs) is
`index.is_none() && prepared.encoding.is_none()`. Encoded VOD does **not**
require a fragment index, but it still requires positive duration and a valid
recipe. This refines re-review R2's combined `vod_source_unsupported` row:
encoding can solve parameter-set variation, not missing duration. Known
refusals use the pinned negotiation error DTO and tested status. Queueing
preparation is not a promise of immediate playback. Never relabel remux as
video transcoding or silently allow rolling recovery.

**Bounded transient startup:** classify actual codes from `START_NOT_YET_CODES`
in [playstart.rs](../../crates/plurxd/src/playstart.rs). Only retry capacity,
owner-transition, startup-timeout and encoded engine-attestation outcomes
that can clear within the measured budget,
under one deadline and the same native claim. Index generation takes longer
and is explicitly excluded. Proposed maximum wait: 15 seconds, reduced if J0
measures a shorter app timeout; honor retry hints within the remaining budget
with bounded backoff. The initial retry allowlist is `startup_timeout`,
`media_owner_transition` and `transcode_capacity_pending`, plus
`vod_engine_unattested` **for encoded candidates only**. Engine capture can
return this native 503 while attestation is in progress after restart; it does
not require rolling fallback. J0 measures restart-to-attestation readiness and
requests a transcode within the first minute after restart. Keep the same
bounded deadline and terminal-failure rules if readiness takes too long;
record the measured distribution before changing the budget. Other codes need
an explicit measured ruling. Share the in-flight result across duplicate GETs.
Do not
assume foreign players implement native retry semantics, pass intermediate
503s through, invent a playable manifest or hold a request indefinitely.

On deadline exhaustion, return a final mapped failure (proposed HTTP 504 for
media entry), reconcile/clean up the claim and record its terminal outcome.
The receipt distinguishes internal retries from the eventual client-visible
failure. Disconnect cancels that waiter's work without discarding another
requester's shared activation. Pin concurrency/admission bounds and prove
there are no detached unbounded retry tasks.

| External event | Accepted evidence | Forbidden inference |
|---|---|---|
| Playing/start | Authenticated intent and reported initial position | Decoded/rendered first frame |
| Progress | Reported source position and explicit pause state | Loaded buffer, throughput or presentation health |
| Ping | Client still references the play | Render progress or an unlimited producer lease |
| Segment/range request | Demand/delivery of those bytes under native VOD rules | Viewer watched them or seek completed |
| Pause | Explicit hold intent, if sent | No future request can arrive |
| Stop | Terminal intent for the exact play | Authority to end every device or a newer play |

Advertise no prepared actions and never acknowledge replacement/buffer/first-
frame evidence on a foreign player's behalf. The existing VOD clock may reap
an idle session during a long pause; J0 and A10 must pause for more than 300
seconds and demonstrate that the client's next real playlist/segment request
resurrects the idle-reaped route correctly. Terminally released sessions must
remain non-resurrectable. Source inspection proves the possibility, not that
a particular player will make the right request.

### 7.3 Event ordering and late traffic

Jellyfin events may not carry a monotonic source sequence. A server-generated
arrival counter orders receipt, not the client's intent. Do not claim it can
detect every delayed progress event or distinguish a backward seek from a
stale packet. Preserve legal backward seeks; never use `max(position)` as
watch-state ordering.

Associate events with the strongest identity the actual client sends. Require
play/source/device matches where present. When a client omits `PlaySessionId`,
resolve only an unambiguous authenticated active binding; do not guess across
multiple plays. J0 must demonstrate whether either target requires this
fallback and record what ordering cannot be guaranteed.

Use server-owned attempt/terminal fences for operations the server can order.
A late stop for a retired play cannot kill its successor. A terminal binding
cannot reopen from a duplicate event or old URL. Retain terminal tombstones
for a bounded, measured replay window. A new explicit negotiation can create
a new play. Stop/ActiveEncodings and retry races need concurrent tests, not
only sequential response snapshots.

### 7.4 Watch protection and final commit are new work

The old draft overstated native protection. A manual edit invalidates the
coalescer's trailing pending beat, but a newly arriving online `put_progress`
is authoritative and can restore the position or watched flag. Only trailing
CAS flushes are protected. Native online semantics remain unchanged by this
compatibility effort; do not claim that reusing `put` solves late progress.

**Compatibility-only fence:** snapshot the user's item-level manual-edit
revision in the play binding. Discard compatibility progress/final writes
whose revision predates the latest manual edit, while still releasing their
resources. Use a transactional monotonic revision, not wall-clock comparison:
same-second edits and multi-node clock differences must not bypass it. Every
manual watch/unwatch path updates that bookkeeping inside `Store::set_watched`
and `Store::set_watched_tree`, in both backends and in the same transaction as
the edit, for every affected item. Native/Plex production callers already use
the tree method; future callers must not bypass the bump. This is new shared
storage bookkeeping, but only compatibility writes enforce the new rule.
A new explicit play captures the current revision and may report progress.

**Own-edit exemption:** a compatibility manual edit from the same authenticated
token and device may advance the unambiguous active binding's revision in the
same Store transaction as the edit/revision bump. It must still match the
pre-edit revision: an already externally fenced binding cannot revive itself
through this exemption. Thread trusted origin context into the shared Store
operation; other callers default to no exemption. A claimed DeviceId alone
is insufficient. Do not advance other devices, terminal bindings or ambiguous
matches. Edits from another app/token/device continue to fence old plays.

Do not relabel already queued pre-edit progress with the new revision. Carry
its original provenance to the commit-time check and discard it there. New
post-edit observations may use the advanced binding, so unwatch-mid-play can
resume saving progress and mark-played-then-Stopped can reconcile normally.
The protocol cannot distinguish an old packet first arriving after the edit
from a genuinely new observation when both have the same identity. That
remaining intra-play ambiguity is best effort, not a causal-order guarantee.

Carry the play/revision provenance through coalesced pending writes. Check the
revision atomically at the eventual Store write as well as at ingress; a check
only before enqueue leaves a race with a later manual edit. J3 must account
for the existing shared `(user, item)` coalescer: compatibility provenance
cannot be lost when beats from different native/compatibility devices mix.
Add cross-node and equal-timestamp regressions before calling the fence built.

**Proposed `put_final`:** one serialized, forced commit for one user/item/play,
not a scoped spelling of global shutdown `drain()`. It reconciles the supplied
final position and invalidates older pending beats for that play under the
same entry lock and Store revision predicate, then updates the coalescer's
committed baseline. It must not erase another play's newer pending value.
Preserve native watch-transition/Trakt/activity effects through the extracted
watch service. Plex `/:/timeline` currently skips some effects; that is the
precedent to avoid, not an implementation to copy.

A stop without position does not write zero. Duplicate stop is idempotent;
a stop discarded by the manual-edit fence acknowledges the terminal play
without changing newer user state. If the final position should apply, a
success response requires its durable result. Storage failure must still
admit bounded cleanup but cannot be reported as durable watch success. Keep
terminal reconciliation state for retry. Progress ordering within a live play
remains best effort as §7.3 explains; this revision fence orders manual edits,
not packets or two concurrent viewers' intent.

Release follows native retirement grace. Test eventual reclamation against
those deadlines for app termination, lost network, token-only logout,
revocation and disabling compatibility. Stop acknowledgement is not a promise
that scratch disappears synchronously.

## 8. Cluster, settings and operational behavior

### 8.1 Cluster guarantees are narrower than client failover

Login/item/source IDs remain stable across nodes and restart. Any ingress node
can authenticate and route an active play. Switching ingress must not create
a duplicate encoder or change watch identity. Relay deadlines, retry hints,
ownership epochs and resource budgets remain the coordinator's rules.

Initial release does **not** promise a foreign app can move to a different
server address after its configured node disappears. Use a stable reachable
ingress for cluster tests. Prove owner-loss behavior separately: the result
may be supported recovery or an honest player error followed by resumable
reconnect. In either case, no split owner, resource leak or watch corruption
is acceptable. Record continuity as measured, never infer it from replicated
session storage.

### 8.2 One explicit switch with advisory readiness

Proposed replicated setting: `compat.jellyfin.enabled`, initially false.
Follow `library_channels.enabled` end to end: `store::keys`,
`stored_switch(value, false)`, `SettingsDto`, `UpdateSettings`,
`has_non_live_tv_update`, and `/api/v1/developer/readiness`. This new auth
surface is opt-in even though the older Plex façade has no equivalent switch.
Settings → Developer displays “Jellyfin client compatibility”, the connection
URL and which acceptance evidence remains. Saving true succeeds regardless
of advisory readiness. Follow [the Developer lifecycle](../features/SETTINGS-NAVIGATION-AND-DEVELOPER-STATUS.md)
and include `devGraduation` at card creation, with the exact “Leaves Developer
when … Then …” acceptance and destination. Add the card function to the
`composedBody` test list; creating a helper that the test never composes is
not coverage. Graduation also updates the Developer audit row.

When disabled, new identity probes, logins, negotiation and media activation
under this façade return JSON 404 from the always-registered nest, including
artwork; no SPA 200 HTML or phantom discovery response. Existing plays may send stop
and finish only the existing bounded retirement policy; they receive no new
lease renewals. Fence in-flight activation against disable and arrange cleanup
of existing compatibility plays. Native accounts, tokens and native playback
remain usable. Re-enabling retains entity IDs and watched state.

After J6 acceptance, move the permanent switch to the appropriate server/
connection settings section, following the current navigation structure.
Name that section in the graduation change rather than creating an unrelated
settings redesign. No new readiness gate or hidden feature flag.

### 8.3 Diagnostics and support

Log a bounded client family/version, route template, normalized error code,
selected delivery reason and non-secret correlation ID. Hash or redact play
capabilities and token-like fields. Metrics use finite route/outcome labels;
never label by arbitrary device, user, item or play ID. Distinguish unsupported
route/profile, authentication failure, source refusal, capacity wait and
owner failure so a generic client “cannot play” is diagnosable.

Operator documentation names the tested versions, connection URL, supported
operations and known limitations. Report compatibility version separately
from Plurx build version. If clients require a Jellyfin-shaped product/version
field, use only the tested protocol baseline and retain clear Plurx identity
in server naming/diagnostics; never announce the newest upstream version merely
to pass a check.

## 9. Milestones — one effort branch, reviewable units

Use `effort/jellyfin-compat`, with `codex/jellyfin-jN-<subject>` task branches
based on the current effort. Milestones share routing, auth and storage files;
integrate serially. No independent-file exception. Proposed ownership means
the task that changes a shared file owns its tests and docs in that commit.

| Milestone | Owned work / likely files | Dependency | Acceptance |
|---|---|---|---|
| J0: trace and lifecycle spike | Pinned manifest, minimized fixtures and disposable prototype outside maintained source unless promoted with tests | This review disposition | Both required apps connect/browse; direct play; Infuse 8.5+ transcode request, Android TV HLS/remux flow; image/subtitle/stream carriers; unindexed copy versus transcode outcomes; missing-duration refusal; restart attestation timing and first-minute transcode; transient startup retries; track/quality renegotiation without old Stopped; pause over 300 s; revised estimate |
| J1: identity and service seams | Pure crate; incarnation/tombstone migrations and binding-to-native-route mapping; auth/watch service extraction | J0 | Concurrent and reused IDs safe; library deletion retires every descendant item/file mapping on both backends; source guard rejects mapped-table REPLACE writes; token-only logout/replacement preserves reader grants; no native behavior regression |
| J2: connect and browse | Flat router entry and child handlers, DTOs/catalog/artwork, settings/readiness, version-implied ancillary routes | J1 | Both apps finish full library paging and navigation; cold-node first-sync and post-materialization artwork coverage measured; deduplicated bounded misses proved; no SPA fallback; advisory switch works |
| J3: direct play and watch | Direct adapter, events, new revision bookkeeping/fence and per-play `put_final` | J2 | Both apps play/seek/stop/resume; cross-client state; Store-level edit revisions; own-edit continuation and external-edit fencing; mixed coalescer provenance tests; two simultaneous devices |
| J4: negotiation and transcode | Single-profile translation; native claims; per-create VOD-only policy through worker routing; predictable negotiation refusals, bounded transient startup and mount aliases | J3; J0 R1/R2 experiments settled | Real-client transcode/remux; no rolling allocation for compatibility; retry/replay creates once; alias redaction; copy fallback without index wait, encoded VOD without index, bounded capacity deadline and long-pause recovery |
| J5: tracks and protocol completion | VTT/audio selection; named additional formats only if required; complete observed ancillary routes | J4 | Track/source/seek matrix and real HDR-grade guard; every mandatory target flow works; WebSocket added only if observed necessary |
| J6: qualification and graduation | Replay/physical/cluster receipts, operational docs, validation scope, final setting placement/audit row | J5 | Every required §10 row passes on frozen candidate; limitations published; findings resolved and promotion receipt recorded |

**J0 failure rule:** missing hardware is “not tested”. If a required client
cannot exercise its promised transport, passive VOD cannot survive the measured
pause/resurrection flow, or a new native recovery policy appears necessary,
amend this contract before J4. An unindexed encoded VOD request with positive
duration, a valid recipe and available capacity must start; missing index
alone is not a valid refusal. Test copy fallback separately, with no index
wait. Neither case may enter rolling recovery and die at 30 seconds.
Do not build the entire route list before these experiments settle the hard seam.

J0 needs the same pinned compiler loop as later Rust work. Delete owned
prototype scripts, upstream downloads and raw captures after retaining the
minimal fixture/findings; do not leave them in the repository.

### 9.1 Repository mechanics are part of J1/J2

The route inventory parser follows `.nest("/jellyfin", jellyfin::router())`
into `http/jellyfin.rs`. Keep that flat zero-argument router entry and place
handlers in child modules. A directory-only module currently fails the parser,
and `router(state)` may be silently missed. If a different router shape is
necessary, extend and test the inventory parser first; never omit the façade
from the inventory. Add each endpoint to `http_route_group` and the API docs.

Cover the new crate and test paths in `validation/points.toml`. Register
Developer composition, readiness, defaults and update fields as §8.2 requires.
Test the disabled mount/unknown route against the actual SPA fallback. A new
Web asset, if needed, still requires all shell-map registrations in §11.

## 10. Acceptance matrix — behavior, not endpoint count

Each test receipt records exact source tree, protocol baseline, client/build,
device/OS, fixture hashes, command or interaction, expected/actual result and
log location. Required rows cannot graduate as “probably works”.

| ID | Scenario | Required result | Evidence level |
|---|---|---|---|
| A01 | Wrong/oversized login, per-node throttle, expired token, token replacement, logout and cross-node revocation | Native expiry/proof rules retained; no stale-token media; unrelated native reader grants survive façade logout | Router/concurrency tests plus real sign-out/relogin |
| A02 | User/body/query mismatch; source from another item; unauthorized media/subtitle; unknown/retired image ID | No cross-user mutation or media leak; image behavior exactly matches declared §5.1 policy | Adversarial router tests |
| A03 | First sync, 2,500 synthetic items, tied sort keys, multiple pages, repeated sync | Complete static catalog exactly once; bounded requests/ID writes; measure consistent auth-read cost of image/range storms | Fixture tests and both apps |
| A04 | Movie, multi-season show, multiple files, artwork change, latest/resume/next-up | Correct hierarchy, source choice and cache invalidation | DTO/query tests and both apps |
| A05 | H.264/AAC MP4 direct play; nonzero resume; forward/back seek | Correct range handling and timeline; no encoder allocated | Both physical clients |
| A06 | Two devices on one account, same item, one stops | Other device keeps playing; no identity collision | Concurrent tests and both devices |
| A07 | Stop then resume in other app; same-token/device unwatch mid-play then progress, mark-played then Stopped; external edits and delayed progress | Durable final state; own edit advances only eligible active binding; external edits fence across nodes; queued pre-edit beats retain old revision; mixed native/compat pending beats safe | Cross-node tests and cross-client run |
| A08 | Forced bitrate-limited transcode and incompatible-container remux | Truthful single-profile flags/URLs; Android TV decodes HLS; Infuse 8.5+ selected transcode flow measured | Planner tests plus a real requesting client |
| A09 | HLS GET retried concurrently and on another ingress; caller disconnects during create | One native claim/attempt, including beyond 60 s; deterministic fingerprint; no orphan producer/reservation | Fault/concurrency tests |
| A10 | VOD pause over 300 s then resume; background/app kill; separate unindexed copy/transcode; missing duration; first-minute post-restart transcode and engine attestation; transient capacity delay; replayed ping | Idle resurrection works via real client request; terminal stop never resurrects; no rolling allocation or invented presentation; copy takes immediate valid fallback/refusal, encoded VOD starts without index when other prerequisites hold; encoded attestation retries bounded with measured readiness/failure; other transient retries bounded | Fake-clock/fault tests and required device runs |
| A11 | Transcoded track/quality switch without old Stopped; seek successor and late old stop/progress/segment requests | Exactly one producer per player after supersession; old traffic cannot kill/revive/alter successor; other devices independent; backward seek legal | Concurrency tests and physical seek run |
| A12 | Two audio tracks, text subtitle on/off, stream index zero, nonzero start then seek | Correct selection and subtitle timing; no duplicated origin | Both physical clients; declared format-specific limits |
| A13 | HDR/DV source and missing/unsupported claims; bitmap subtitle selection | Legal native policy outcome; no fabricated capability; current session-grade burn refusal preserved | Planner regressions plus a supported physical HDR path |
| A14 | Base path, TLS proxy, observed image policy, authenticated subtitle/media and nested aliases | Resources reachable; alias capabilities redacted; no token/internal-address leak; disabled/unknown mount returns JSON 404 | Router tests and both clients |
| A15 | Switch off during create/play, re-enable; storage unavailable during stop | No new activation/renewal after disable; bounded cleanup; no false durable success; IDs retained | Race/fault tests |
| A16 | Alternate ingress, owner loss, restart, rolling-version mismatch | No duplicate owner, wrong-user state or leak; continuity classified honestly | Isolated cluster test plus client observation |
| A17 | Native/Plex login, browse, direct/HLS playback after shared extractions | Existing behavior retained | Focused existing regression suites |
| A18 | Schema upgrade, library cascade deletion, highest-row-ID reuse and old executable rollback | Deterministic retirement triggers cover all descendants on both backends; mutation-proved guard rejects REPLACE writes to mapped tables; retired wire IDs never resolve replacements; additive tables/triggers remain through rollback | Both supported Store backends and upgrade fixture |

A13 need not prove every HDR format. It must prove the supported outcome for
the chosen corpus, including refusal where appropriate. A16 does not add a
promise of seamless node failover; §8.1 defines the narrower release contract.

### 10.1 Automated tests to retain

Pure tests cover JSON shape, IDs, checked ticks, null/absent distinctions,
query precedence, profile predicates and source/stream mapping. Router tests
cover exact statuses, auth, response envelopes, nested media URLs, ranges and
resource limits. Actor/store tests cover concurrent activation, stale events,
stop reconciliation, expiry, ownership transfer and both storage backends.

Replay fixtures prove request/response compatibility and state transitions,
not just that a URL returns 200. Mutation checks should show the tests fail
when a relevant constraint, auth check, activation fence or origin conversion
is deliberately removed. Use focused mutations for these high-impact seams;
do not add trivial tests that merely repeat constant values from the code.

### 10.2 Commands and evidence must name real tests

Before editing Rust, follow [the compile-loop instructions](../ci/AGENT-COMPILE-LOOP.md).
Verify the repository pin rather than trusting the shell's default cargo.
Source-only cloud compilation uses committed `git archive` content and carries
neither `.git` nor a credential. Re-run against the exact integrated base.

```bash
rustc --version                            # repository pin: 1.97.1
make effort-rust-check                    # formatting and all-target compile
make lint                                 # workspace Clippy, denied warnings
make unit-core                            # core library + replicated-store tests
python3 -m unittest discover -s tests/operations -p 'test_docs_index.py'
python3 -m unittest discover -s tests/operations -p 'test_api_doc_routes.py'
node tests/web/settings-sections.test.js
```

`make unit-core` does not run daemon tests. After J1 creates the crate, run
its actual unit target and the affected `plurxd` tests. Each milestone also
names the smallest relevant daemon/store regression by real path and test
name. Commands for proposed tests are intentionally not presented as if those
tests already exist. Confirm test count/output so a filter matching zero tests
does not become evidence. Bare core tests without `hiqlite-store` do not
qualify replicated storage.

## 11. Integration, documentation and promotion

1. Publish this docs-only contract as a separate draft PR from current main
   before J0, including both reviews and their dispositions. The publication
   base is `9a719fcb7`; unrelated local commits/edits are excluded. Start the
   implementation effort from then-current main, bring in the reviewed docs
   if still unmerged, and re-verify the named source seams.
   Establish the pinned compiler loop and current effort base before Rust
   edits. Keep the normal tracked pre-commit hook; it does not replace focused
   tests or effort compile evidence.
2. Each task PR targets the current effort. User-observable behavior subjects
   use `fix(` or `perf(` under [AGENTS.md](../../AGENTS.md), with real
   `Regression-Test: <path>::<test name>` lines preserved in landing messages.
3. Treat `Effort development gate` as blocking. It permits integration, not a
   compatibility release. Record focused local evidence in the task PR.
4. Follow the current [development pipeline](../DEVELOPMENT_PIPELINE.md),
   including draft main-bound PR handling and its prescribed adversarial
   review. Do not invent a new CI workflow, recurring schedule or deployment
   process for this feature.
5. Freeze task merges, integrate current main, and qualify the resulting exact
   candidate. Promotion requires `Main promotion gate` and its qualification
   receipt. If the candidate or base moves, requalify; older evidence describes
   another tree. Merge alone does not deploy.

Implementation updates the live [API](../API.md), [client strategy](../CLIENTS.md),
[feature inventory](../FEATURES.md), [operations](../OPERATIONS.md) and relevant
[security](../SECURITY.md)/[playback](../PLAYBACK.md) contracts in the commits
that introduce behavior. Add route inventory coverage, not only prose. New
documents and evidence belong in their subject folders and receive index rows
in the same commit. Read [the web shell map](WEB-SHELL-LAYOUT.md) before adding
any settings asset; new assets need the source, `WEB_ASSETS`, shell tag and map
row together.

Do not claim the complete ecosystem is supported in the README. Publish the
tested app/version/operation matrix, how to connect, how to disable, and known
limits. Rollback disables the façade and retires its resources; additive ID
tables remain so re-enabling does not invalidate client libraries. Do not
delete user watch state or rotate native user credentials as rollback.

## 12. Opus review disposition — adopted choices and remaining evidence

The [first review](JELLYFIN-COMPATIBILITY-REVIEW.md) approves the architecture.
The [re-review](JELLYFIN-COMPATIBILITY-REREVIEW.md) approves starting J0. This
revision incorporates B1–B5, M1–M6, R1–R8 and third-review S1–S4; R2 is qualified by the source
requirement for positive duration even on encoded VOD.
The choices below are the revised proposal, not a claim that Paul separately
ruled on each one or that J0 has passed.

| Decision | Revised contract | Still to prove |
|---|---|---|
| D1: first clients | Infuse ≥8.5 + official Jellyfin for Android TV; Findroid optional direct-only | Exact releases/devices and real transcode/remux requests |
| D2: baseline | Pin server/schema/client revisions and version-implied ancillary calls | Response shapes and actual feature-dependent calls |
| D3: IDs | Random UUIDs with native incarnation and permanent retirement/non-reuse protection | Deterministic AFTER DELETE triggers and full library cascade tests on both backends |
| D4: binding | Compatibility identity/manual revision/terminal mapping; reuse MediaSessionRoute and native activation claims | Expiry, owner transfer and bounded writes |
| D5: lifecycle | Passive VOD, no rolling fallback; immediate copy fallback, encoded VOD without index, bounded transient startup including encoded engine attestation | Separate copy/encode, first-minute post-restart transcode, missing-duration/capacity behavior and >300 s physical pause/resume |
| D6: consistency | Existing batched authoritative watch reads with `read_after=None` | Alternate-node read-after-write and first-sync auth cost |
| D7: activation | Stable player digest → playback_id; PlaySessionId plus delivery digest → request_id; mount-relative HLS aliases | Renegotiation without old Stopped, one producer, missing IDs and late replay |
| D8: watch writes | Store-level manual revision plus same-token/device own-edit advance; per-play `put_final` | Queue provenance, external-edit fences, own-edit continuation and failure cleanup |
| D9: event order | Server-owned lifecycle fences; intra-play packet order remains best effort | Actual ambiguous flows and narrowed promises |
| D10: extras | Trace version-implied routes; deduplicated artwork miss materialization; WebSocket only if demonstrated necessary | Required bootstrap replies, image credentials and cold-node first-sync coverage |
| D11: subtitles/HDR | Current session-grade burn guard; VTT first | Requested formats and actual HDR route outcome |
| D12: estimate | J0 4–6 engineer-days; useful release 5–9 engineer-weeks cumulative | Re-estimate from J0, especially preparation and watch-write work |

Copyable review request:

```text
Review docs/clients/JELLYFIN-COMPATIBILITY-BUILD.md against current Plurx
source. Read docs/clients/JELLYFIN-EMBY-COMPATIBILITY-OUTLINE.md for scope.
This is a design review, not authorization to implement or deploy it.

Prioritize correctness of authentication and user isolation, GUID identities,
profile constraints, lazy/idempotent playback creation, external-client
lifecycle without fabricated buffer/render facts, stale event handling,
watch durability, cluster routing, and the realism of the client matrix.
Identify mandatory client requests the candidate endpoint list misses.
Distinguish source-proven defects from questions J0 must measure.

Read the retained review and its disposition; distinguish remaining gaps from
corrections already applied. Return findings in severity order with document section, concrete failure
scenario, supporting source references and a specific amendment. Answer
D1–D12, identify release-blocking design gaps, and give a scoped revised
estimate if necessary. Avoid rewriting the whole plan unless its core
architecture is wrong. Do not treat a successful HTTP response as proof
that a physical client can play, seek, stop or resume.
```

## 13. Upstream references and verification limits

Inspected on 2026-10-02. These are primary sources for protocol/client facts;
they are not acceptance receipts. J0 replaces moving references with pinned
versions and minimized fixtures used by the build.

- [Jellyfin MediaInfo controller](https://github.com/jellyfin/jellyfin/blob/master/Jellyfin.Api/Controllers/MediaInfoController.cs):
  negotiation, query/body precedence and device-profile fallback.
- [Jellyfin Playstate controller](https://github.com/jellyfin/jellyfin/blob/master/Jellyfin.Api/Controllers/PlaystateController.cs):
  playback start/progress/stop, ping and watched-state route variants.
- [Jellyfin UserViews controller](https://github.com/jellyfin/jellyfin/blob/master/Jellyfin.Api/Controllers/UserViewsController.cs):
  current and legacy user-view routes.
- [Jellyfin Kotlin SDK](https://kotlin-sdk.jellyfin.org/guide/getting-started.html):
  client/device identification, login, discovery and WebSocket event support.
- [Jellyfin transcoding](https://jellyfin.org/docs/general/post-install/transcoding/):
  client capability profiles and server-selected compatible output.
- [Jellyfin Android](https://github.com/jellyfin/jellyfin-android):
  why the official mobile web-wrapper is a separate scope from Findroid.
- [Infuse 8.5 release](https://community.firecore.com/t/infuse-8-5-smarter-streaming/60410)
  and [Jellyfin Android TV](https://github.com/jellyfin/jellyfin-androidtv):
  revised primary clients; exact request behavior still needs capture.
- [Findroid source](https://github.com/jarnedemeulemeester/findroid):
  optional direct-play-only coverage and version-dependent ancillary features.
- [Jellyfin image controller](https://github.com/jellyfin/jellyfin/blob/master/Jellyfin.Api/Controllers/ImageController.cs):
  unauthenticated item artwork behavior that motivates the explicit image policy.

The author has not captured either client's traffic, selected a reference
server release, run a prototype, verified a physical client, or demonstrated
external lifecycle behavior. Those are J0's work. This document specifies how
to obtain that evidence and what the subsequent build must preserve.
