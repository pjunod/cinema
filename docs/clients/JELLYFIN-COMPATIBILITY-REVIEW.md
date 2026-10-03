# Jellyfin compatibility build contract — review

**Status:** review delivered · **Verdict:** architecture APPROVED; contract
CHANGES REQUESTED · **Written:** 2026-10-02 EDT · **Reviewed at:**
`f1f1390f1` (`origin/main` on the forge at review time)

This reviews `docs/clients/JELLYFIN-COMPATIBILITY-BUILD.md` and its outline,
both from commit `bafa05725` on your local branch `codex/playback-seek-30s`.
That branch is not on the forge, and neither is the doc's stated base
`4d725701`. Every source claim below was re-checked against current `main`,
read from my own clone. Upstream client facts come from the projects'
own pages (linked). No code, branch, setting or push was made.

## 1. Verdict

The overall shape is right:

- a façade over the existing auth, catalogue, watch, planner and session
  owners;
- a pure protocol crate plus an HTTP adapter;
- no second planner and no second scanner;
- J0 is a trace-and-lifecycle spike, and nothing gets built before it.

D5 is the seam the doc worried about most. On the **VOD path** the answer is
yes:

- Library HLS production is driven by segment requests (`prodsched`, 180 s
  ahead horizon).
- The session idle clock is 300 s, and media GETs renew it
  (`vodserve.rs:72`, `vod/serve/delivery.rs:266`).
- Control requests are optional.

So a passive client needs no invented buffer or render facts there.

The contract falls short in five places:

1. **Neither chosen Android client can request a transcode or remux, and
   Infuse can only do it from 8.5 onwards** (B1).
2. **A passive transcode on an unindexed title is killed at about 30 s**
   (B2).
3. **The artwork authentication rule contradicts the protocol the clients
   speak** (B3).
4. **The watch-state protection it says it reuses does not exist** (B4).
5. **Several "reuse the existing service" rows misdescribe the source** (B5).

None of these changes the architecture. All of them change J0's acceptance
criteria and J3/J4's scope.

## 2. Findings

### B1. The client pair cannot exercise the required routes

**Findroid.** Its upstream README lists **"Direct play only, (no
transcoding)"**
([source](https://github.com/jarnedemeulemeester/findroid)). It can never
produce A08's forced transcode or its incompatible-container remux.

**Infuse.** It has always direct-played. Infuse **8.5 (July 2026)** added
server transcoding for Plex, Emby and Jellyfin, but only as a user choice: a
long-press on Play opens a list of "available transcoded versions", or a
Settings → Playback prompt
([Firecore](https://community.firecore.com/t/infuse-8-5-smarter-streaming/60410)).
How it asks the server for that list is undocumented. Firecore staff
declined to say, and users report that Jellyfin doesn't even show those
plays as transcodes
([thread](https://community.firecore.com/t/jellyfin-not-reporting-transcoding-when-selected-in-infuse/60456)).

**What that leaves.** At best, one client can transcode (Infuse ≥ 8.5),
through a request shape nobody has seen. No client can remux. J0's own
failure rule would trip on day one.

**Amendment:**

- Pin Infuse at ≥ 8.5.
- Make the official native **Jellyfin for Android TV** app the
  Android acceptance client. It is Kotlin/ExoPlayer, it sends a
  `DeviceProfile` and it consumes server HLS.
- Keep Findroid only as an optional direct-play phone row.

The swap also produces the Android TV row the outline defers to P2, on
hardware you already own. Correct the §1.1 table, the D1 answer and the
§13 reference list.

### B2. A passive transcode on an unindexed title dies at about 30 s

When VOD refuses a create, `TranscodeManager` falls back to a live-recovery
**rolling** session (`transcode/manager/create.rs:480-530`).

The rolling actor's startup budget works like this:

- It arms when the first video playlist is served (`playback_control.rs:11610`).
- It terminates the session as `StartupExpired` after
  `ROLLING_PRESENTATION_STARTUP_BUDGET = 30 s` (`:90`, `:8999-9006`,
  `:12085-12096`).
- Only `observe_control` can set `Presented` (`:9075`).

A Jellyfin client never sends control. So every passive play that lands on
the rolling fallback ends about 30 s in.

This is not rare. The reference-film review found lab6 with **310 ready vs 909 queued**
fragment-index files under the current engine digest. Most titles take the
fallback after any engine change.

The same create path also answers the `START_NOT_YET_CODES` with 503
(`playstart.rs:162`): `vod_index_pending`, `transcode_capacity_pending`,
`startup_timeout`, and others. Native clients retry on a 1/2/4 s ladder.
AVPlayer and ExoPlayer treat a 503 on a master playlist as fatal.

**Amendment, before J4:**

1. In §7.2, name the passive rolling policy explicitly. Pick one:
   - **(a)** The façade never accepts the rolling fallback. It serves
     progressive remux, or returns an honest unsupported result.
   - **(b)** Media-GET demand counts as presentation evidence for passive
     sessions. That is a native policy change, and by the doc's own rule it
     must be split out and reviewed.
2. The façade absorbs not-yet codes itself: hold the GET under a bounded
   deadline and retry internally. It must not pass a 503 through to the
   player.
3. Add a J0 case and an A10 row for a pause longer than 300 s on VOD. The idle
   reap is resurrectable through the durable route, but a foreign player's
   next playlist GET has to actually take that path.

### B3. Artwork authentication contradicts the protocol

Jellyfin's `ImageController` has no class-level `[Authorize]`, and neither
does `GetItemImage` (`GET/HEAD Items/{itemId}/Images/{imageType}`). Only the
mutations carry `RequiresElevation`
([source](https://github.com/jellyfin/jellyfin/blob/master/Jellyfin.Api/Controllers/ImageController.cs)).
Clients build image URLs themselves, so there is no server-issued URL that
could carry a capability.

The doc requires authorization for artwork in three places: §5.1 ("applies
again to … artwork"), A02 and A14. If neither client attaches a token,
posters go blank.

There is also nothing to authorize against beyond authentication. Plurx has
no per-user library permissions; native `/api/v1/images/{filename}` checks
only `AuthUser` (`http/images.rs:335`).

**Amendment:**

- Make this a ruling (§4, ruling 2).
- J0 records whether each client sends `api_key`, `X-Emby-Token` or nothing
  on image, subtitle and stream GETs.
- Change "authorized libraries" in §4.2 to "supported libraries", since
  every authenticated user sees every library.

### B4. The native watch protection §7.4 relies on does not exist

§7.4 says: *"Manual watched/unwatched actions retain native timestamp/CAS
protections. Do not let late compatibility progress undo a later manual
unwatch."* Natively, a late progress beat **does** undo a manual unwatch:

- **Divergence reset.** When `ProgressCoalescer::put` sees the durable row
  move, it clears `last_commit` (`progress.rs:174-181`). That makes the next
  beat due immediately.
- **Unconditional write.** That beat goes through `put_progress`, an
  unconditional online upsert: `watched = watch_state.watched OR
  excluded.watched … WHERE ?7 = 1 OR …` (`store/sqlite/watch.rs:305-317`,
  and the same in `hiqlite_media.rs:3903`). It restores the position, and
  re-marks the item watched at ≥ 95 %.
- **Only the trailing beat is protected.** Only the trailing coalesced flush
  is CAS-protected (`put_progress_if_current`).

Two related gaps:

- **No per-key flush.** `drain()` flushes every key and is called only at
  shutdown (`main.rs:2986`).
- **Plex follows the forbidden pattern.** Plex `/:/timeline` calls only
  `progress.put` and skips the watched and Trakt effects (`plex.rs:341-383`).
  That is exactly what §2 forbids, so Plex is the precedent to avoid.

**Amendment:**

- State plainly that the rule is new behaviour. Either accept native
  semantics or add a compat-only fence: discard progress for a play binding
  created before the user's latest manual watch edit. The binding already
  carries a creation time.
- Specify Stopped as a forced commit for one key (a new `put_final` that
  clears that key's pending value), not as a scoped `drain`.

### B5. The "reuse" table misdescribes four seams

1. **`authenticate_token` vs `user_for_token`.** `authenticate_token` is a
   `Store` method (`store/mod.rs:2303`), not part of `extract.rs`.
   `user_for_token` wraps it and **does** honour expiry and revocation
   (`:2308-2313`). What `AuthUser` adds (`extract.rs:705-742`) is the typed
   `session_expired` 401 and the cache-only admin-proof bookkeeping. Reuse
   that extractor body.
2. **`api_key`.** In Jellyfin, `api_key` *is* the user access-token carrier.
   Native Plurx accepts Bearer, `X-Api-Key` and `?token=`, and has no
   `api_key` anywhere. Rewrite the rule: `api_key` is a user-token carrier,
   and the façade refuses `plx_` scoped keys (`ScopedKey`,
   `extract.rs:575-611`).
3. **Logout side effects.** Native logout revokes **every** file grant the
   user holds (`auth.rs:445`). A Jellyfin logout from Infuse would kill
   reader grants on all the user's devices (ruling 5).
4. **Login service.** Login is entirely inside the axum handler. Throttle,
   capacity, `client_ip` and the password work are module-private, so the
   extraction is real work. The throttle state is per node (`AppState`),
   which A01's "cross-node" wording should acknowledge.

Also cite **`crates/plurx-compat-plex`** (`map.rs`, `xml.rs`, `gdm.rs`) as
the existing precedent for the proposed pure crate. The doc never mentions
it.

### Major (non-blocking for J0, must be fixed in the contract)

**M1. §6.1's "validate, then disqualify the candidate" has no next
candidate.**

What the planner offers:

- `DeviceCaps` has per-codec video caps, but `audio`, `containers` and
  `transports` are flat lists (`caps.rs:227-312`).
- `decide()` returns one `Decision`.
- The only override is `decide_forced(Auto|Original|Transcode)`
  (`playback/mod.rs:1358, 1585`).

**Amendment:**

- Evaluate direct play in the pure crate: Jellyfin `DirectPlayProfiles` and
  `CodecProfiles` against the probed source facts.
- For anything else, build `DeviceCaps` from **one** chosen
  `TranscodingProfile`, which is a single container and codec tuple. Then
  call the transcode or remux path, validate its output, and refuse on a
  mismatch.
- With that structure no union is ever formed.

**M2. §6.2 already has a home.** The native create already has the
idempotency machinery the doc asks for:

- `playback_id` for supersession;
- `request_id` with a 60 s durable claim;
- an intent fingerprint that returns 409 when the same key arrives with a
  different intent;
- 410 once released, and a replay of the stored response
  (`hls/create.rs:1817-1983`).

Map `PlaySessionId` to `playback_id`, and the normalized delivery key's digest
to `request_id`. Build `CreateSession` deterministically so retries
fingerprint identically. Never route through the deprecated bridge. It is
confirmed to synthesize `legacy:{user}:{file}`, and its own docstring
acknowledges the two-device collision (`hls/control.rs:3-60`).

**M3. URL topology.**

- **HLS must be aliased under the mount.** Jellyfin clients concatenate
  their base URL with `TranscodingUrl` or `DirectStreamUrl`. The native
  `playlist_url` is root-absolute, `/api/v1/hls/{id}/index.m3u8`
  (`media_sessions.rs:5955`), so mount a capability-path HLS alias under
  `/jellyfin`. The inner URIs are relative (`init.mp4`) and follow
  automatically.
- **Redaction.** Add the alias to the path redaction in `safe_trace_target`
  (`http/mod.rs:2334-2349`, case-sensitive).
- **No public-origin setting exists.** `trusted_proxies` only resolves the
  client IP. Keep every URL relative and drop "configured public origin"
  unless J0 proves a client needs it.
- **Disabled paths answer 200 HTML.** Unknown paths fall to the SPA
  (`web.rs:579-589`), so a disabled `/jellyfin/...` currently returns HTML
  with status 200. The nest must always be registered and must return a
  404 JSON body while the switch is off.

**M4. D6 is already solved for watch state.**

- **Watch reads are already authoritative.** `bounded_watch` with no
  `read_after` always reads from Authority (`store/mod.rs:5694-5717`,
  `watch_fence.rs`). Read the UserData the façade needs through that path,
  batched per page. Don't build a session floor.
- **Auth cost to measure.** Hiqlite `authenticate_token` is a consistent
  read on every request (`hiqlite.rs:4724-4746`). Measure the image and
  range storm during an Infuse first sync as part of A03.

**M5. The advertised server version selects client behaviour.**

- Findroid gates trickplay on 10.9 and media segments on 10.10.
- The Kotlin SDK flags unsupported and outdated server versions.

So the version §8.3 reports defines the endpoint surface. §4.2 needs a column
of what the pinned version implies the client will call. An empty
`MediaSegments` list is a legitimate answer; a 404 may not be.

**M6. Repository mechanics the plan will trip on.**

- **Route inventory.** `test_api_doc_routes.py:117-150` only follows
  `.nest("/x", module::router())` and `.merge(fn())`, reading
  `http/<module>.rs`.
  - A directory module `http/jellyfin/` raises FileNotFoundError.
  - A `router(state)` call is silently left out of the inventory.
- **Route groups.** Every route also needs an `http_route_group`
  classification (`http/mod.rs:229-233`).
- **Settings test.** The new Developer card must be added to the
  `composedBody` list (`settings-sections.test.js:470-515`).
- **Validation scope.** The new crate's paths need `validation/points.toml`
  coverage.
- **Switch precedent.** Copy `library_channels.enabled` end to end:
  - the key in `store::keys`;
  - `stored_switch(.., false)`;
  - `SettingsDto` and `UpdateSettings`;
  - `has_non_live_tv_update`;
  - `/api/v1/developer/readiness`.
- **Commands.** In §10.2, use the repo's own targets: `make
  effort-rust-check`, `make lint` and `make unit-core`. Note that
  `unit-core` covers only the plurx-core library, not the plurxd tests.

### Minor

- **§6.3, burn consent.** `subtitle_burn_sdr` is no longer load-bearing
  (`hls/create.rs:1677-1687`). The real guard is
  `burn_would_discard_this_session_hdr`, which returns 422
  `hdr_subtitle_burn_refused`. Rewrite the paragraph against that guard.
- **§6.3, text subtitles.** Native text subtitles are WebVTT only, and a
  failed extraction is a 500 (good). Any SRT or ASS request needs a format
  decision in J0.
- **§6.3, direct streaming.**
  - It has no `If-Range`, ETag or 304 support (`stream.rs:3225-3228`).
    Either drop "conditional requests" or scope them as new work.
  - Multi-range requests serve the first range only.
- **§5.2.** Tombstones are confirmed necessary: every entity table is `id
  INTEGER PRIMARY KEY` with no AUTOINCREMENT on both backends, so the
  highest id can be reused. Files keep their id across rescans through the
  path upsert. Derive the server ID from `instance.id` with the dashes
  removed. Note that Plex deliberately advertises the node ID.
- **§4.2 `/Users/Public`.** No native enumeration policy exists. Always
  return an empty list and drop the "unless".
- **Login token rows.** Jellyfin replaces the prior token for the same
  DeviceId on login. Decide whether to mirror that so repeated Infuse
  re-auths don't pile up token rows.
- **Downloads.** Findroid, if it is kept, exposes downloads.
  `/Items/{id}/Download` must refuse honestly.
- **§8.2.** The Developer card must carry its "Leaves Developer when … Then
  …" line at creation ("Paul chooses" is allowed). Graduation adds an audit
  row. The Plex façade has no switch, so justify the difference in one line:
  an opt-in for a new authentication surface.

## 3. D1–D12

| | Answer |
|---|---|
| D1 | No as written. Use Infuse ≥ 8.5 plus Jellyfin for Android TV; Findroid optional (B1). |
| D2 | Yes, and larger: the advertised version selects the endpoints (M5). |
| D3 | Keep the replicated random-UUID table. A deterministic hash of a reusable rowid inherits the reuse problem. Tombstones are required. |
| D4 | Reuse `MediaSessionRoute` for owner, epoch and terminal state. The compat binding needs only its own identity, activation and tombstone rows. |
| D5 | Yes for VOD. No for the rolling fallback until a policy is chosen (B2). |
| D6 | Already solved by the Authority-default watch read (M4). |
| D7 | Yes, via the existing `playback_id`/`request_id` claim (M2). Missing `PlaySessionId` still needs J0 traces. |
| D8 | Needs a new per-key final commit plus an explicit unwatch fence (B4). |
| D9 | As proposed. Server fences cover stop and successor races; progress ordering stays best effort. |
| D10 | No WebSocket is needed by any target that has been checked. Add the version-implied ancillary routes (M5). |
| D11 | Policy unchanged; rewrite against the real guard (minor, §6.3). |
| D12 | J0: 4–6 days (third client, passive-rolling experiment). P1: 5–9 engineer-weeks, mostly B2 and B4. |

## 4. Rulings for Paul (my picks; override freely)

1. **Client set.** *Pick:* Infuse ≥ 8.5 (tvOS) plus Jellyfin for Android TV.
   Findroid optional for phone direct play.
2. **Artwork authentication.** Choose between anonymous image GETs under
   unguessable UUIDs (Jellyfin parity) and token-required, which accepts
   blank art on clients that send nothing. *Pick:* anonymous for images
   only, decided after the J0 traces. Streams and subtitles stay
   authenticated unless traces force otherwise.
3. **Passive rolling fallback.** *Pick:* (a) the façade never takes the
   rolling fallback. It uses progressive remux or an honest refusal, with no
   native policy change.
4. **Late progress after a manual unwatch.** *Pick:* a compat-only binding
   fence, leaving native behaviour untouched. Native has the same hole; that
   is a separate decision.
5. **Logout scope.** *Pick:* a façade logout revokes its own token only, not
   every file grant the user holds.

## 5. Amended order

1. Rebase the docs commit onto current `main` and apply the amendments above.
2. Run J0 with the new client set. Required receipts:
   - the Infuse 8.5 transcode request shape;
   - the Android TV HLS flow;
   - which carriers each client attaches to images, subtitles and streams;
   - an unindexed-title transcode under ruling 3;
   - a pause longer than 300 s.
3. J1/J2 as planned, plus the M6 mechanics.
4. J3, with B4's final commit and fence.
5. J4, gated on the B2 policy.

---

## 6. First author disposition — superseded where noted by re-review

**Reconciled:** 2026-10-02 · **By:** Codex · **Documentation base:**
`4e81bc38c` · **Source rechecked:** review tree `f1f1390f1`

Sections 1–5 retain the supplied Opus review with the node and media title
sanitized to `lab6` and “reference film”. Finding labels are unchanged. The source
attachment SHA-256 is `2a3a7c798089e9228ea038f877564b5917d48bb306ea2edc89576582f06b8cc0`.
This section is the author's response. The hash identifies the original attachment, not this sanitized document.
Sanitization does not turn proposed rulings into user approval. The revised
[build contract](JELLYFIN-COMPATIBILITY-BUILD.md) and
[whole-effort outline](JELLYFIN-EMBY-COMPATIBILITY-OUTLINE.md) are the operative
plans. No implementation or physical acceptance is claimed.

This is the first reconciliation, retained as history. The
[re-review disposition](JELLYFIN-COMPATIBILITY-REREVIEW.md#5-author-disposition--r1r8)
supersedes its B2/index wait, M2/player mapping, own-edit and docs-publication
choices. The current build contract is authoritative.

### 6.1 Finding-by-finding disposition

| Finding | Disposition | Contract change / evidence |
|---|---|---|
| B1: client set | Accepted | §1.1 uses Infuse ≥8.5 and official Android TV; Findroid optional/direct-only. Upstream README and Firecore release independently checked. J0 owns the actual transcode request capture. |
| B2: rolling fallback and startup | Accepted with bounded-failure clarification | §7.2 uses existing passive VOD and forbids compatibility rolling allocation per request. Absorb transient not-yet outcomes within a measured deadline, then return an honest terminal failure. Do not promise every unindexed file can start or universally assume a player's HTTP retry behavior. A10 requires >300 s pause/resurrection. |
| B3: artwork | Accepted as an explicit J0 policy decision | §5.1 separates anonymous mapped artwork from authenticated media entry and issued capabilities. UUIDs are not ACLs. J0 traces carriers and records the chosen image policy; no native image policy changes. All authenticated users currently share supported library visibility. |
| B4: watch protection | Accepted, strengthened against races | §7.4 calls this new work. Use a transactional manual-edit revision rather than a timestamp alone; carry the fence through delayed coalesced commits. Add per-play `put_final`, with queue provenance and shared-service side effects. Native online conflict semantics remain unchanged. |
| B5: reused seams | Accepted | §2/§5.1 correct Store lookup semantics, typed expiry/proof behavior, real login extraction, per-node throttle, `api_key` user-token transport and scoped-key refusal. Façade logout revokes only its token/plays, leaving unrelated reader grants. Pure Plex crate linked. |
| M1: no native candidate iterator | Accepted | §6.1 matches direct profiles against source plus native admission; non-direct planning uses one TranscodingProfile tuple at a time. Refusal or an explicit bounded next-profile attempt replaces the imaginary native candidate loop. |
| M2: existing request claims | Accepted | §6.2 maps play/delivery identities to native playback_id/request_id, deterministic fingerprints, replay and terminal outcomes; test the 60-second claim boundary. |
| M3: URL topology and disabled routes | Accepted | §3.1 specifies mount aliases, relative URL joins, capability redaction and always-registered JSON 404 routes. No fictional public-origin configuration or SPA fallback. |
| M4: watch authority and auth cost | Accepted | §5.3 reuses batched watch_map with no read_after and therefore Authority. No new session floor. A03 measures consistent auth-read cost. |
| M5: advertised version | Accepted | §4.1/§4.2 require a version-implied-call column in the pinned inventory, including optional trickplay/media-segment behavior and legitimate empty replies. |
| M6: repo mechanics | Accepted | §9.1 uses a flat zero-argument router entry plus handler children, complete route groups/inventory and validation scope; §8.2 covers settings fields/composition/readiness; §10.2 uses repository targets and separate daemon tests. |
| Minor: burn guard and subtitles | Accepted | §6.3 uses the actual session-grade guard, not the obsolete acknowledgement bit; VTT is the existing text format, with other requested formats settled in J0. |
| Minor: direct ranges | Accepted with precision | The reviewed implementation treats If-Range as a full-response request; it does not offer strong validators/304. It validates a bounded multi-range list and selects the first satisfiable member, not necessarily the first literal member. §6.3 states these semantics. |
| Minor: row reuse, public users and login rows | Accepted | §5.2 adds incarnation/retirement and transactional deletion/cascade coverage; server ID is logical instance.id. Users/Public is always empty. §5.1 specifies bounded replacement of same-device compatibility tokens, preserving other tokens. |
| Minor: downloads and Developer lifecycle | Accepted | Optional Findroid downloads refuse honestly; card creation includes graduation text and composition tests; graduation adds its audit row. New auth surface is explicitly opt-in. |
| Rebase docs onto main | Applied as an implementation-base requirement, not a shared-history rewrite | This checkout contains unrelated commits and uncommitted work. Do not rebase it wholesale. Keep the docs revision scoped here; start the implementation effort from then-current main and port only the relevant docs. Source facts above were independently read from the review SHA with git show. |

### 6.2 Evidence and limits of the reconciliation

The author inspected the review tree's VOD TTL/delivery touch, production
horizon, rolling startup state, create fallback, request claims, watch upsert
and coalescer behavior, AuthUser/Store token semantics, logout scope, authoritative
watch reads, HDR burn guard and range implementation. Repository inventory
parsing and build targets were also read. The cached origin/main at inspection
was `15e36f7f4`; the checked auth/progress/create/VOD/fallback files had no diff
from `f1f1390f1` there. This is a source comparison, not a claim to have fetched
or qualified the latest remote main.

The review's deployed fragment-index counts are retained as reviewer-supplied
context and were not remeasured. Its client HTTP-failure observations do not
prove all versions fail identically. The build therefore requires actual
preparation/timeout and pause measurements in J0. No client traffic or physical
playback was captured during this documentation revision.

### 6.3 Proposed rulings carried into the build

1. Infuse ≥8.5 plus Jellyfin for Android TV; Findroid optional.
2. Decide image auth from J0 evidence. Anonymous mapped item artwork, if
   required, is an explicit opt-in-surface consequence; native routes and
   media authorization remain unchanged.
3. No rolling fallback for compatibility; compatible progressive remux or
   bounded preparation followed by an honest failure. Do not change native
   presentation policy.
4. New compatibility-only manual-edit fence, implemented with durable
   revisions and commit-time checks. Native online semantics are separate.
5. Token-only compatibility logout with cluster proof invalidation; no
   unrelated reader-grant revocation.

These are build-plan choices awaiting J0 evidence where noted, not recorded
rulings from Paul. Revised planning estimate: J0 4–6 engineer-days; useful
release 5–9 engineer-weeks cumulative. No new code, deployment, exposed route,
anonymous endpoint or changed account policy is authorized by this record.
