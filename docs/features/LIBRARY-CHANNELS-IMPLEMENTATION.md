# Library channels — turn a subject into a schedule on every client

**Status:** ready to build; not built ·
**Executes:** Paul's request for subject channels made from existing media,
including web and mobile, coordinated with the native Live TV redesign ·
**Written:** 2026-09-09 · **Intended implementer:** Sol

This is the complete first-release handoff. Read §1–§4 before writing code,
implement the contracts in §5–§11, and deliver the work packages in §13.
The scope covers the server, web, iPhone/iPad, Android phones/tablets, Apple
TV, and Google TV. Creation and editing on phones are part of the release.

Companion to [the native Live TV layout plan](LIVE-TV-NATIVE-LAYOUTS-IMPLEMENTATION.md)
(the shared visual and navigation conventions), [PLAYBACK.md](../PLAYBACK.md)
(existing finite-media delivery), [ARCHITECTURE.md](../ARCHITECTURE.md)
(the Store and cluster boundaries), and [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md)
(how this effort reaches main).

The feature direction and mobile scope came from Paul. The concrete defaults
below are this handoff's engineering/product decisions, not claims that Paul
separately approved every control or limit. Implement them unless newer user
direction supersedes them. Keep any necessary deviation explicit in this
document; do not substitute a playlist, omit a client, or redesign the tuner
transport to make the task smaller.

The inspiration is [Airwave's channel model](https://www.getairwave.tv/docs/channels):
a media selection and an ordering produce an always-on schedule. Plurx owns
its catalogue and player already, so this feature adds a schedule, not a
dependency on Airwave or Plex. The earlier conversation mockups used fictional
titles and are exploratory; this document and the approved layout plan define
behavior. No external mockup file is needed to implement this plan.

## 1. Deliver subject channels that behave like television

A viewer creates **Deep Space** from selected documentaries and matching
catalogue metadata. Plurx shows what is on and what follows. If two devices
tune in at 20:20, both request the programme scheduled at 20:20 and the
corresponding source offset. Nobody must have left a player running at 20:00.

The viewer can instead choose **Watch from start**, which opens that title as
ordinary personal playback. The shared channel continues advancing. **Return
to channel** rejoins whatever is scheduled then, not the old file or offset.

### 1.1 The release includes every surface

| Surface | Required delivery |
|---|---|
| Server | Durable channel definitions, preview, deterministic schedules, permissions, favourites, playback context, bounded refresh and failure behavior. |
| Web | Browse, now/next, guide, tune, restart, return, create/edit, item selection, preview, enable/disable and delete. |
| iPhone / Android phone | The same functional creation/editing and viewing scope; touch layouts and a vertical schedule in portrait. |
| iPad / Android tablet | The mobile feature set with side-by-side creator preview and a usable full guide when space permits. |
| Apple TV / Google TV | Browse and watch using all three approved TV layouts, favourites, programme details, source-specific playback actions. Creation/editing belongs on web/mobile. |

The feature is available without a tuner, a configured Live TV source,
external metadata service availability, an AI key, or an experimental flag.
An empty Library channels page explains how to create the first channel.

### 1.2 Keep the existing Live TV delivery bounded

Use a **Library channels** entry beside the existing **Live TV** entry in the
established navigation. Preserve the current Live TV name and routes. Reuse
its presentation components, with an explicit source identity, rather than
forking a second guide implementation. This release does not merge the two
lineups or rename the app's navigation to Channels.

This is a deliberate default: the Live TV task approved three TV layouts,
not a combined-source navigation redesign. A future combined guide can use
the source identity without changing the stored library-channel IDs.

Library channels inherit the approved TV presentation preference:
**Guide + preview** (default) · **Guide over picture** · **Channel browser**.
Changing that preference changes presentation only. A phone uses its own
On now / Guide preference, not the TV layout selector.

### 1.3 Explicit non-goals keep this build finite

- No tuner DVR, recording, rewind, scheduled tuning, or changes to tuner
  ownership and leases. A local file's restart capability proves nothing
  about a tuner stream.
- No AI/embedding service, automatic downloads, or remote catalogue lookup
  while creating a channel. Subjects are transparent local selection rules.
- No new general collections/playlists subsystem, studio/cast metadata
  enrichment, parental-control system, or per-library permission framework.
- No music, audiobooks, ebooks, photos, or home-video channels in this release.
  Movies and episodes provide a bounded first scheduling contract.
- No daypart programming, wall-clock appointments, advertisements, bumpers,
  channel export through M3U/XMLTV/Plex compatibility, or public sharing.
- No promise of frame-accurate synchronization, gapless switching, new HDR
  support, or a new background encoder. Use current delivery capabilities.
- No cross-tab player dock or new mobile background playback policy. Preserve
  the platform's existing PiP and departure/cleanup contract.

## 2. Start from the actual repository boundaries

Source inspected at checkout HEAD
`4d05857f07e7792c4c6e80ce237074b9567ecfb8`, with unrelated working-tree changes
and the native layout plan present. Re-verify these anchors on the intended
integration base; line numbers and migration counters will move.

| Existing boundary | Verified fact and implication |
|---|---|
| [domain.rs](../../crates/plurx-core/src/domain.rs) | `Item` has `id`, `library_id`, `kind`, `parent_id`, `title`, `year`, `overview`, `genres`, and `tags`. `MediaFile` has an independent ID, probed duration, size, mtime, and media facts. Schedule files, not provider-estimated runtimes. |
| [items.rs](../../crates/plurxd/src/http/items.rs) | Metadata editing currently refuses non-home libraries. Channel rules and manual membership must not write tags into movie/show records. |
| [store/mod.rs](../../crates/plurx-core/src/store/mod.rs) | Durable work belongs behind the Store trait family, with SQLite and Hiqlite implementations. There is no established channel store to call. |
| [replicated.rs](../../crates/plurx-core/src/store/replicated.rs) | Replicated SQL binds application-computed time/random values. Conditional writes must check affected-row outcomes. |
| [hiqlite_import.rs](../../crates/plurx-core/src/store/hiqlite_import.rs) | Import has an explicit table/column census. Adding tables without import/backup accounting loses user definitions during migration. |
| [extract.rs](../../crates/plurxd/src/http/extract.rs) and `User` | Existing identity is an authenticated user with `is_admin`; there is no rating/child-profile ACL in the inspected `User` model. Do not advertise one. |
| [http/mod.rs](../../crates/plurxd/src/http/mod.rs) | Tuner sessions and finite-file sessions have separate routes. Extend finite-file playback for library channels. |
| [hls.rs](../../crates/plurxd/src/http/hls.rs) | `POST /api/v1/files/{id}/hls/sessions` takes `CreateSession`, including `playback_id`, `request_id`, `start` in seconds, capabilities, and `presentation: "vod"`. Other presentations are refused. |
| [watch.rs](../../crates/plurxd/src/http/watch.rs) | `/items/{id}/progress` updates watch state, touches direct-play activity, feeds Trakt, and emits watched notifications. A source offset near the end is not evidence that a channel viewer watched the whole title. |
| [playstart.rs](../../crates/plurxd/src/playstart.rs) | `note_playback_started` can announce actual playback to Trakt separately from progress. Suppressing progress alone is insufficient. |
| [web/live-tv.js](../../crates/plurxd/src/web/live-tv.js) and [web/index.html](../../crates/plurxd/src/web/index.html) | Reuse their guide/display conventions; isolate new channel logic in a module instead of continuing to enlarge the main HTML script. |
| [LiveTvView.swift](../../clients/apple/Sources/LiveTvView.swift) and [LiveTvScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvScreen.kt) | These are the current native browse surfaces; the concurrent redesign may extract components before this effort integrates. |
| [PlayerView.swift](../../clients/apple/Sources/PlayerView.swift) and [PlayerScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/player/PlayerScreen.kt) | Finite-media players already own lifecycle, next-episode behavior, track selection, and progress. Channel mode must explicitly arbitrate these behaviors. |

All interfaces, tables, route additions, types, and constants introduced in
the rest of this document are **proposed additions**, not existing API names.
Use the source links above to find the current integration seam.

## 3. Decide ownership and visibility before building the creator

### 3.1 Personal means one account; shared means all signed-in accounts

Every channel has an immutable owner user ID and `visibility` of `personal`
or `shared`. Default to `personal`. Every signed-in account can create,
read, edit, disable, and delete its own personal channels. Only administrators
can create shared channels or change a personal channel to shared.

Administrators may manage all channels through a management surface. Ordinary
browse lists include one's own personal channels and enabled shared channels;
being an administrator does not insert other users' personal channels into
the viewing lineup. An admin management listing must be explicit.

Only the owner or an administrator can mutate a channel. Another account's
personal channel returns 404 on browse, guide, preview, resolution, and tune
requests. A visible shared channel that the caller cannot edit returns 403
on mutation. Machine API keys do not become user identities for this feature.

Delete channels and favourites when their owner account is deleted, within
the account-deletion contract. Test delete/recreate of an account so reused
numeric user IDs cannot inherit old personal channels. Do not reassign the
deleted owner's shared channels implicitly.

### 3.2 Sharing never expands media access

Apply the server's current media authorization at preview, guide detail,
resolution, session creation, and restart. The inspected base has account
authentication, not a parental-rating system; this release does not invent
one. If the integration base gains finer restrictions, use that enforcement
and render a restricted programme without leaking its metadata or changing
the shared schedule. Never filter a shared timeline differently per viewer.

Favourites are private per `(user_id, channel_id)`, durable, and idempotently
set/unset. Do not reuse the tuner's lineup favourite bit as a user preference.

## 4. A channel recipe selects real catalogue items

### 4.1 Offer three entry points into one editor

1. **Create channel** starts with a subject template or an empty selection.
2. **Make a channel** on a movie/series detail starts with that movie or the
   series' episodes explicitly included. A series expands through its seasons.
3. **Selected titles → Create channel** starts with explicit movie/show/episode
   IDs. This is also the way to create a director/studio theme when catalogue
   fields cannot express it.

An empty template is editable; it does not fabricate matches. Suggestions
such as Space documentaries, '90s comedy, and Film noir are local presets
over available fields. Show the exact rules after choosing one. Do not claim
that a lexical search semantically understands a subject.

### 4.2 Use a bounded recipe, not arbitrary query code

Example create body; numeric IDs are illustrative catalogue IDs:

```json
{
  "request_id": "caller-generated-uuid",
  "name": "Deep Space",
  "description": "Astronomy, exploration, and the worlds beyond Earth.",
  "visibility": "personal",
  "enabled": true,
  "recipe": {
    "version": 1,
    "library_ids": [3, 4],
    "kinds": ["movie", "episode"],
    "genres_any": ["Documentary"],
    "tags_any": [],
    "keywords_any": ["space", "astronomy", "mars"],
    "year_min": null,
    "year_max": null,
    "include_item_ids": [],
    "include_show_ids": [],
    "exclude_item_ids": [],
    "exclude_show_ids": [],
    "ordering": "balanced_shuffle",
    "include_specials": false,
    "auto_refresh": true
  }
}
```

All nonempty rule dimensions are ANDed; values within an `*_any` dimension
are ORed. Keywords match a case-normalized literal substring in title or
overview, not regex, SQL wildcards, filenames, or external text. Trim and
deduplicate inputs. Use one Rust matching implementation for both stores;
SQL may narrow candidates but cannot change Unicode/case semantics between
backends. Escape parameterized LIKE prefilters if used.

For episodes, genre comes from the containing show; tags and keyword text
use the union of episode and show metadata. Year filtering uses episode
`air_date`'s year, then episode `year`, then show `year`; unknown fails a
bounded-year filter. Movies use their own metadata. Return match reasons
with field/source, e.g. `show.genre: Documentary`, so inheritance is visible.

Resolve show ancestry through the actual show/season/episode hierarchy.
Do not assume an episode's direct parent is always a show. Refuse cyclic or
invalid ancestry and report excluded rows in preview diagnostics.

Selection precedence is exact:

```text
 scoped eligible movies/episodes matching every active rule
                         UNION
 explicitly included movies/episodes + expanded included shows
                           │
            intersect selected libraries and supported kinds
                           │
     subtract explicit excluded items and expanded excluded shows
                           │
        remove ineligible/unprobed/nonpositive-duration files
                           ▼
                 deduplicated candidate pool
```

All-empty rule dimensions match nothing unless explicit inclusions exist;
do not accidentally select the whole server. An explicit "All movies in
this library" preset sets library and kind deliberately. Add an internal
`match_all_in_scope` Boolean, default false, to represent that affirmative
choice rather than inferring it from missing fields. The example above
omits it and therefore uses false.

Inclusions override subject filters, but never exclusions, library scope,
authorization, supported kind, or file eligibility. Specials (season 0) are
excluded unless `include_specials` is true, even for an included show.
Unknown season/episode ordering excludes that episode with a reason in v1;
do not guess an order that plays a finale before a pilot.

### 4.3 Pin one file and an honest duration per item

Use probed `MediaFile.duration_ms`, positive and at most 24 hours, with
successful probe and a video stream. Do not use `Item.runtime_ms` to allocate
slots. Pick one file per item: retain the current generation's eligible file
when possible, otherwise the lowest eligible file ID. This stable choice
avoids node/device-dependent durations. Display the chosen edition in preview.

Do not inspect the NAS, open a media file, or spawn ffprobe while filtering.
Preview proves catalogue eligibility; playback's existing availability/open
path proves bytes can be read. Pin ID, size, mtime, and duration in the
generation. A changed file is not silently substituted during a scheduled
occurrence. Its replacement is eligible for the next generation.

### 4.4 Limits and preview are part of the contract

| Bound | First-release value |
|---|---|
| Name / description | 1–80 / 0–500 Unicode scalar values, with a 64 KiB total request-body cap. |
| Channels | 50 owned per account; 200 total server channels, enforced transactionally. |
| Libraries per recipe | 32. |
| Strings per rule dimension | 32; each 1–100 Unicode scalar values. |
| Explicit IDs | 1,000 total across include/exclude arrays, unique positive IDs. |
| Eligible pool | 10,000 items; exceeding the limit is an explicit error, never silent truncation. |
| Candidate evaluation | At most 100,000 catalogue rows per build, paged at 500; advise narrowing scope if exceeded. |
| Preview page | Default 50 items, maximum 100, opaque cursor scoped to recipe/content digest. |
| Concurrent builds | Two per process, with one replicated claim per channel and 200 queued channel IDs maximum. |

These are implementation limits, not measured capacity claims. Keep them in
one named constants module and expose useful validation errors. Preview
returns eligible title count, total unique-play duration, reasons for
inclusion/exclusion, the first ten scheduled entries, and the pending
activation time if editing. A 225-minute pool is described as "3h 45m before
the rotation repeats," not as a channel that can avoid repeats for two days.

## 5. Persist immutable rotations; derive the clock without a running stream

### 5.1 The schedule is a repeating ordered vector

A published generation owns an ordered vector of selected item/file pairs,
each with a positive duration. It also owns a UTC epoch and an algorithm
version. An occurrence is `(channel_id, generation_id, cycle, ordinal)`.

The first generation starts at the server-computed publication timestamp;
there is no historical channel before that epoch. A draft with no eligible
pool has no active generation. Publishing its first valid generation is the
same initial-activation operation, not a replacement with an invented old
rotation boundary.

For lengths `d[0..n)`, cumulative offsets `p[0] = 0`,
`p[i+1] = p[i] + d[i]`, and `L = p[n]`:

```text
elapsed = server_utc_ms - generation_epoch_ms
cycle   = floor(elapsed / L)
offset  = elapsed mod L
ordinal = the i satisfying p[i] <= offset < p[i+1]
start   = generation_epoch_ms + cycle * L + p[ordinal]
end     = start + d[ordinal]
position_in_file = server_utc_ms - start
```

Use checked signed 64-bit millisecond arithmetic, binary search over prefix
offsets, and half-open `[start, end)` intervals. Reject nonpositive loop
duration and overflow. Before the epoch the channel has no programme; do not
emit negative cycles. API timestamps are UTC Unix milliseconds; localized
time and DST never affect scheduling.

Server time here means the serving host's UTC clock, not a new clock-consensus
service. Cluster hosts must keep their clocks synchronized; the §12
two-device measurement records inter-node skew and assumes it is at most
500 ms. A backwards/forwards host clock adjustment is handled by re-resolving
on the next clock refresh. Do not promise tight agreement between hosts with
unsynchronized clocks or try to repair it with client wall-clock arithmetic.

No durable row is written for each repeat or each clock tick. Derive guide
occurrences from the vector only for the requested window. No channel
encodes, opens media, or owns a playback session merely because it exists.

### 5.2 Ordering version 1 is deliberately reproducible

`balanced_shuffle` creates one queue per show, ordered by season, episode,
then item ID; each movie is a one-item queue. Order queues by SHA-256 of a
domain separator, the stored 32-byte seed, and a length-delimited stable
queue identity. Take one item from each nonempty queue per pass until all
queues are empty. Store the resulting order; do not shuffle on the client.

Every selected item appears once before the rotation repeats. Shows keep
episode order across the full rotation. A short show's first episode is not
replayed while a longer show still has unseen entries in that same rotation.
At rotation wrap every show's queue begins again. The same generation repeats
the same order; per-cycle reshuffling is not a v1 feature.

`release_order` uses the same ordered per-show queues but repeatedly selects
the queue head with the earliest effective year; unknown years sort last,
ties use item ID. This keeps episode order even where catalogue years are
odd. The editor names it "Release year; keep episodes in order."

The seed is created once outside replicated SQL and persisted. Refreshes
reuse it. An explicit **Reshuffle** generates a new seed and a new generation,
with the activation behavior in §5.3. Store `algorithm_version = 1` so later
code does not reinterpret an existing channel's order.

### 5.3 Edits do not rewrite what people are watching

Maintain an effective generation and at most one scheduled replacement.
Normal rule edits and automatic membership refreshes activate **at the next
full rotation boundary**. Show the exact date/time before saving; a large
pool can make this days away. This preserves the current order and avoids
unexpected early repeats. Name/description changes are immediate.

Offer an explicit **Apply after this programme** action for an editor who
wants the new selection sooner. Compute the next occurrence end on the
server, schedule the new generation there, and explain that this begins a
new rotation and may repeat recently played titles. No Apply now action cuts
off an active programme. Empty/invalid replacements never replace a working
generation.

Compute the activation boundary at publication, not when a slow build starts.
Require at least 30 seconds of lead time; if a boundary is closer, use the
following appropriate boundary. A later edit replaces a pending generation
only with a guarded revision update; the client sees the resulting time.

Disabled channels have no tune action. Disabling keeps the generation and
epoch, so re-enabling rejoins the schedule's current time. A published
disable/delete revokes following sessions at their next normal control
exchange; personal playback detached earlier may continue under ordinary
media permissions. Deletion does not delete media.

### 5.4 Automatic refresh reads the catalogue, not the media disk

Enqueue a channel after relevant catalogue mutation, with 30 seconds of
debounce. Reconcile enabled auto-refresh channels every 15 minutes to recover
missed events; allow no more than one automatic build per channel per 15
minutes. Manual preview/save may run sooner within the bounded builder pool.
Queue saturation coalesces work by channel ID and leaves reconciliation to
recover it; it never drops a successful user save silently.

Hash canonical membership, pinned file fingerprints/durations, seed, ordering,
and algorithm version. Publish only if that content changes. Recheck recipe
revision and source snapshot consistency before publication; a stale builder
cannot overwrite a newer edit. If catalogue consistency cannot be established
within two bounded retries, report `catalogue_changed` and keep the old
generation rather than producing a mixture of two catalogue states.

Make the consistency strategy explicit in the Store implementation: use a
stable catalogue snapshot where supported, or introduce a monotone catalogue
selection revision advanced transactionally by the relevant item/file/library
mutations. In the revision approach, read the revision before and after the
paged candidate/ancestry read and require equality; fence publication against
that same revision. A pair of content hashes from unconstrained paged reads
does not prove a coherent snapshot. Reuse an existing equivalent revision on
the integration base if one is present. Include deletes and probe/metadata
updates in its mutation census, not just scanner additions.

If `auto_refresh` is false, the active membership stays fixed until an editor
explicitly rebuilds. Missing files can still make an occurrence unavailable.

## 6. Storage and publication survive restart and failover

### 6.1 Add one channel domain to both Store implementations

Add a `LibraryChannelStore` trait to the Store composition, using domain
types outside HTTP DTOs. Proposed method responsibilities:

```text
list_library_channels(actor, management, cursor, limit)
get_library_channel(actor, channel_id)
create_library_channel(actor, request_id, request_hash, definition)
update_library_channel(actor, channel_id, expected_revision, definition)
delete_library_channel(actor, channel_id, expected_revision)
set_library_channel_favourite(user_id, channel_id, favourite)
claim_library_channel_build(channel_id, expected_revision, claim)
stage_library_channel_entries(claim, bounded_entries)
publish_library_channel_generation(claim, expected_revision, publication)
read_library_channel_generation(actor, channel_id, generation_id)
list_library_channel_refresh_candidates(cursor, limit)
prune_library_channel_generations(before_ms, bounded_limit)
```

All mutation authorization and revision predicates must hold at the write
boundary, not merely in an HTTP read performed before the write. Use a typed
Applied/Stale result; a zero-row CAS cannot be reported as success.

### 6.2 Required durable entities and invariants

The exact migration number must come from the integration base. The following
is the required logical schema; adapt SQL spelling to both existing backends
without changing semantics:

| Entity / primary key | Required columns |
|---|---|
| `library_channels` / `id TEXT` | owner user ID, name, description, visibility, enabled, definition revision, recipe JSON, seed, active generation ID/epoch, nullable pending generation ID/epoch, created/updated UTC ms. |
| `library_channel_generations` / `id TEXT` | channel ID, definition revision, algorithm version, content digest, state (`building`/`ready`), entry count, loop duration ms, build claim ID/expiry, created UTC ms. |
| `library_channel_entries` / `(generation_id, ordinal)` | item ID, file ID, file size/mtime, duration ms, cumulative start ms, show ID if relevant. |
| `library_channel_favourites` / `(user_id, channel_id)` | created UTC ms. |
| `library_channel_requests` / `(user_id, request_id)` | operation/body digest, channel ID/result revision, created/expiry UTC ms. |

Enforce unique `(generation_id, item_id)`, positive duration, nonnegative
ordinal/prefix, valid visibility/state, and uniqueness of request identity.
Account/channel deletion cleans its dependent rows. Generation entries must
**not** cascade away when an item/file disappears: their pinned duration and
identity preserve the schedule, with unavailable metadata resolved safely.
Never persist absolute media paths or user credentials in these entities.

A recipe is canonical bounded JSON, not arbitrary SQL. Use normalized entry
rows rather than placing a 10,000-item vector in one Raft command. Page reads
at 500 rows; stage writes in batches of at most 200 rows and 64 KiB serialized
payload, whichever comes first, further constrained by the current Store/WAL
limit. Builders have a 120-second claim, renew every 30 seconds, and abandon
work when renewal fails. No caller can publish another builder's claim.

Publication atomically verifies ownership/authorization, definition revision,
claim, count, cumulative offsets, total duration, content digest and completed
staging, marks the generation ready, and changes the channel's pointers. Use
guarded SQL predicates/triggers appropriate to Hiqlite's transaction model;
do not emulate rollback by inspecting affected-row counts after committing
unconditional pointer writes. An interrupted build is never visible.

The effective generation is pending when its stored epoch has arrived,
otherwise active. Reads do not need a background promotion write. Subsequent
mutations normalize this pair within their guarded transaction before
installing another pending generation. Guide generation splits the requested
window at the pending epoch, using each vector on its own side of the boundary.

Retain active and pending generations indefinitely while referenced. Retain
superseded generations for 24 hours for bounded retry/diagnostic resolution;
expired build claims are prunable after one hour. Do not serve old occurrence
tokens as permission to start stale following playback. Bound cleanup batches
to 200 rows/64 KiB and cap abandoned builds to one per channel before admitting
another. Idempotency records expire after 24 hours and are bounded to 1,000
per user; fail with an actionable 429 rather than evicting a live key.

### 6.3 All durable-store inventories must change together

Update standalone migrations, replicated schema installation/migration,
replicated SQL validation/census, Store trait composition, import table and
column lists, backup/restore checks, deletion cleanup, and fresh/upgrade
schema parity evidence in the same work package. Add the new tables to
existing snapshot accounting rather than writing a second export format.

Do not guess schema/protocol version integers from this document. Inspect
current migration activation and mixed-version behavior. If an older voter
cannot apply the additive SQL safely, use the existing schema/protocol
compatibility mechanism before publishing channel rows; do not invent a
developer feature flag. A supported upgrade must preserve existing streams,
users, watch state, and schedules. Downgrading to a binary that cannot read
the new schema is not a supported rollback; retain a pre-upgrade backup.

## 7. Expose a small authenticated API

### 7.1 Routes and bounds

All paths below are under `/api/v1/library-channels`. Register them in the
existing router and its route/security/serving-role matrix. New catalogue
reads use current authoritative/eligible-voter rules; learners or unavailable
quorum do not mint new channel playback authorizations from stale local data.

| Method / suffix | Contract |
|---|---|
| `GET /` | Visible channel summaries, private favourite state, effective now/next, source `library`; cursor default 50/max 100. Management listing requires admin. |
| `POST /preview` | Evaluate an unsaved recipe or owned edit without publishing it; return §4.4 preview with a content digest and cursor. No file I/O. |
| `POST /` | Create idempotently using `request_id`; validate and create a disabled/draft definition if no eligible titles. Enabling an empty channel returns 422. |
| `GET /{id}` | Definition, permissions (`can_edit`, `can_delete`, `can_share`), revision, effective/pending generation and refresh status. |
| `PUT /{id}` | Full replacement of editable fields with mandatory `expected_revision` and `request_id`; no lost-update merge. |
| `DELETE /{id}` | Mandatory expected revision; repeated delete by the same authorized request is idempotent. |
| `POST /{id}/rebuild` | `expected_revision`, `request_id`, `activation: next_rotation or next_programme`, optional `reshuffle`; stages a new generation. |
| `GET /{id}/build` | Last build state: queued/building/ready/failed, error code, counts and activation time; never credentials or paths. |
| `PUT /{id}/favourite` | `{ "favourite": true }`; idempotent, scoped to caller. |
| `GET /guide` | `channel_ids` up to 20; default 90-minute window, maximum 24 hours; at most 1,000 occurrences per response with an opaque continuation cursor. |
| `POST /{id}/resolve` | Resolve **now** from authoritative server time; no media session, file open, or watch event. Returns the tune target below. |
| `POST /{id}/sessions` | Validate the resolved occurrence and create a following session through the existing finite VOD session service; never the tuner service. |

Use explicit collection paths in the router rather than letting `/guide` or
`/preview` be parsed as channel IDs. Mutations acknowledge definition storage
and build queuing separately: `202` may mean `build_state: queued`, not that
the channel is playable. Creation retries return the same channel/build result.
A reused request ID with another normalized body returns 409.

Preview cursors bind actor, recipe digest and candidate-content digest. A
changed catalogue invalidates the cursor with 409; the client refreshes the
preview instead of mixing pages. Preview's digest is advisory at save:
re-evaluate, report changed membership, and never trust client counts/file IDs.

### 7.2 A resolved occurrence has a server clock and an expiry boundary

```json
{
  "source": "library",
  "channel_id": "channel-uuid",
  "definition_revision": 7,
  "generation_id": "generation-uuid",
  "occurrence": {"cycle": 2, "ordinal": 8},
  "server_now_ms": 1789000000000,
  "starts_at_ms": 1788999000000,
  "ends_at_ms": 1789002120000,
  "item_id": 42,
  "file_id": 51,
  "position_ms": 1000000,
  "capabilities": {
    "join_schedule": true,
    "watch_from_start": true,
    "seek_while_following": false,
    "record": false
  }
}
```

IDs and occurrence fields are identifiers, not bearer credentials. Revalidate
them at session creation. Server-computed offsets and boundaries win over
client-supplied `start`. Resolve never accepts a caller-selected wall clock
to authorize a past/future slot; the guide is the read-only time-window API.

The following-session request has `generation_id`, `occurrence` (cycle and
ordinal), `tune_sequence`, and `playback`, where `playback` carries the existing
`CreateSession` fields including `playback_id`, `request_id`, caps, tracks and
`presentation: "vod"`. The server obtains the file ID from the occurrence;
the caller cannot swap it. Return the existing `StartResponse` plus the
accepted `library_channel` identity and current occurrence boundaries.

This dedicated route is a compatibility boundary, not a second streaming
implementation. An old server returns 404 instead of ignoring an unfamiliar
optional field on ordinary VOD creation and unexpectedly recording history.
Extract/reuse the existing finite-session application service where needed;
do not call an HTTP handler over loopback or duplicate its lifecycle code.

Responses carry `Cache-Control: private, no-store` for definitions, previews,
resolution, and actor-specific guides. If ETags are later used for list/guide
polling, bind them to actor and definition/generation state; never share a
personal response across authenticated accounts.

### 7.3 Failures are named and leave the previous schedule intact

| Status / code | Meaning and client response |
|---|---|
| 400 `invalid_recipe` | Invalid field, body bound, enum, or combination; focus the field. |
| 401 / 403 / 404 | Existing auth conventions and §3; no leaked personal metadata. |
| 409 `channel_revision_changed` | Reload server definition; keep the unsaved local form for comparison. |
| 409 `catalogue_changed` | Restart preview/build from a consistent view; no partial publish. |
| 409 `channel_occurrence_changed` | Slot/generation changed during tune; resolve now again once. |
| 410 `channel_unavailable` | Disabled/deleted channel; release following session and show a reason. |
| 422 `channel_empty` | No eligible selection; retain draft or old generation. |
| 422 `channel_limit_exceeded` | Narrow the recipe or remove unused channels; no truncation. |
| 422 `scheduled_media_unavailable` | Pinned file missing/changed/ineligible; keep the occurrence's times. |
| 429 `channel_build_busy` | Bounded queue/request ledger full; include Retry-After. |
| 503 `channel_store_unavailable` | Cannot establish authoritative state; retain stale guide visibly, refuse new tune. |

No external metadata service failure is a channel runtime failure: this
feature uses already-stored metadata. Rendering a stale guide never proves
that a tune request is safe to authorize.

## 8. Follow the schedule through the existing finite-media player

### 8.1 Persist playback purpose before starting media side effects

Extend internal finite session creation with an optional typed
`library_channel` context: channel ID, generation ID, cycle, ordinal, and
caller tune sequence. The dedicated channel-session route supplies it after
validation; ordinary VOD creation supplies no channel context. Check it
against the authenticated user, effective channel, file ID, fingerprint, and
current occurrence. The server recomputes the current source start offset.

For following mode, use the existing **finite VOD HLS session path on all
clients**. This gives one durable playback identity and control path on
which to enforce purpose, lifecycle and history suppression. HLS can copy or
remux when supported; it does not imply always transcoding. Ordinary personal
playback retains its existing direct/remux/transcode decision behavior.
Adding direct-range delivery to following mode is deferred until it can carry
equivalent isolated purpose and activity semantics.

Persist the validated channel purpose alongside the canonical media-session
recipe/identity before `note_playback_started` or any producer attachment can
fire. Carry it through owner handoff, recovery, preparation/commit, and
idempotent retry. Audit session serialization/import/migration if this needs
an additive field. Never store it solely in client state or a node-local map.
An unrelated VOD session for the same account/item must remain ordinary VOD.

Control validation checks current channel visibility/enabled state and session
ownership. A routine definition revision change does not invalidate a valid
active-generation session. A slot that naturally ends is resolved by the
following controller; a permissions revocation is enforced on the normal
control exchange. Keep these outcomes distinct from a corrupt session context.

Recovery/ownership transfer must only land on a version that understands and
enforces the stored purpose. Include this requirement in the existing cluster
protocol compatibility contract. If a mixed-version fleet cannot preserve
it, refuse channel-session creation with a server-upgrade reason until the
compatibility condition holds; ordinary VOD/tuner sessions remain available.
Silently dropping channel purpose during recovery is not backward compatibility.

Keep `presentation: "vod"`. Do not resurrect the removed growing-live
presentation, pass library channels into the tuner endpoint, or create a
combined endless HLS manifest by concatenating arbitrary media files.

### 8.2 The player owns one tune intent and one active media session

```text
browsing ── Tune in ──▶ resolving ──▶ starting ──▶ following
   ▲                       │             │            │
   │                       └─ stale/error┘            ├─ boundary → resolve now
   │                                                  ├─ pause → paused
   │                                                  ├─ restart → personal
   └────────── Stop / leave feature ◀──────────────────┘

paused ── Resume live ──▶ resolve now
personal ── Return to channel ──▶ resolve now
```

Use a stable existing `playback_id` per player instance and its canonical
selection/intent ordering. Add a monotone client tune sequence for channel
UI work and reject late resolve/create responses. Do not replace server
intent fencing with the client counter. A stopped/superseded selection cannot
be revived by an old response. Two devices on one account have independent
player IDs and must not supersede each other's sessions.

On a file change, use the existing player/session replacement lifecycle and
its stop, cancellation, and uncertainty barriers. Do not set a second
unmanaged player running under the preview. On a source change between tuner
and library, release through the outgoing controller before committing the
incoming playback; do not invent a cross-source session takeover.

### 8.3 Startup latency must not turn Tune in into permanent time shifting

Estimate server time from the resolve response and request RTT, then advance
that estimate with a monotonic clock. Refresh at each programme boundary,
foreground resume, explicit Return, and at most once per 30 seconds while
following. A device wall clock changed by an hour cannot move the schedule.

The server recomputes the source start at session creation. Once a frame is
ready, compare source position to the estimated live position; seek through
the existing player/control path if more than 2 seconds behind. Account for
`media_origin_ms`; player-local zero is not necessarily source zero. If the
programme changed during startup, discard the stale response and resolve
again instead of displaying the previous title's last seconds.

Bound automatic recovery to one catch-up/re-resolve attempt per startup and
one recovery sequence per 30 seconds. Repeated slow startup produces a
visible Retry/Watch from start state, not an unbounded restart loop. Treat
2 seconds as the acceptance target under healthy local conditions, not a
guarantee of frame-locked viewing over every network.

### 8.4 Programme boundaries follow server time, not autoplay

At a scheduled end, resolve current time and start its occurrence. An early
player-ended event re-resolves; if the same occurrence still applies, render
an ending/unavailable state until its end rather than starting the next
programme early. A late timer resolves now and skips already-expired slots.
When the app wakes after suspension, do not play a queued list of missed titles.

Disable the normal next-episode/autoplay queue while following. Suppress
automatic skip-intro/credits/marker seeks because they would advance a
viewer ahead of the shared schedule. Audio/subtitle/quality preferences still
apply, but select tracks by per-file language/capability preferences, never
by carrying stream indices from another file.

Do not start a successor producer just because a guide cell is focused.
The first implementation may start the successor at the boundary and show a
brief loading state; use existing prepared handoff only where its cross-file
support is actually proved. A seam change is not permission to write a new
player/prewarm protocol. Record transition latency in §12 rather than calling
unmeasured switching gapless.

### 8.5 Pause, restart and failure have explicit meanings

| Action / condition | Behavior |
|---|---|
| Pause following | Pause this player; the schedule advances. Show Paused and **Resume live**, not a misleading shared pause. Keep existing paused lease behavior. |
| Resume live | Resolve current time and reuse/reseek or replace through the controller as required. Never resume an expired programme by accident. |
| Watch from start | Capture the selected/current item, start ordinary personal playback at zero, and retain channel ID only as a Return action. No automatic switch at the channel boundary. |
| Personal title ends | Show Return to channel / Replay; suppress ordinary next-episode autoplay for this detached channel entry so the result is predictable. |
| Return to channel | Resolve now; preserve channel preference but discard old occurrence/position. |
| Missing/changed scheduled file | Show Programme unavailable with next programme/time; keep shared timestamps. Retry through existing bounded recovery; no alternate-edition substitution. |
| Restricted programme | Redact unauthorized details, show unavailable for this account until the next boundary. Do not accelerate that account's schedule. |
| Node dies | Existing media recovery applies; recovered sessions retain channel purpose. Re-resolve after recovery and catch up using the bounded policy. |
| Channel disabled/deleted | Stop following at control validation; show Channel unavailable. A previously detached personal title keeps normal media authorization. |
| Server/store unreachable | Keep existing permitted playback/lease behavior; stop or fail at lease expiry. No locally invented future authorizations. |

There is no seek bar, skip-next, or previous-title action while following.
Watch from start exposes ordinary finite-file seeking. This is a playback
capability distinction, not a layout distinction.

## 9. Channel surfing must not corrupt watch history

**First-release policy: following mode never writes ordinary watch progress,
marks watched, or emits Trakt/monarr watch events.** This holds even if the
viewer stays for the whole programme. It avoids guessing watched coverage
from a late-join position; coverage-based history can be a separate feature.
Explain this once in channel/player Info: "Channel viewing doesn't change
your watch history. Watch from start uses normal progress."

Suppress the following session's progress calls in web/Apple/Android, and
enforce its purpose at server-controlled start/recovery/control side effects.
Do not use a global `(user_id, item_id)` suppression flag: another device may
be watching that same title normally. Extend the start notifier call sites to
accept verified playback purpose, so following never consumes the ordinary
VOD notifier's dedup claim either.

Use the finite session's control exchange for activity and keepalive; do not
call `/items/{id}/progress` merely to keep following visible in Activity.
Show channel name and current programme there using the session context.
Ordinary direct-play activity and existing clients remain unchanged.

An old client that never requests channel context retains current progress
behavior. The legacy item progress endpoint is not itself proof of a channel
session; do not break explicit personal progress by suppressing every beat
for an item currently playing on some channel. New channel clients must
never issue those beats, including final-disappear/error/recovery callbacks.

On Watch from start, create an ordinary playback purpose and use current
progress/scrobble rules. On Return to channel, fence late callbacks and flush
or end personal progress through the normal controller before installing
following mode. Test simultaneous personal/following playback of the same
item and rapid restart/return sequences.

## 10. Reuse the approved layouts with source-specific actions

### 10.1 Share presentation contracts, not transport controllers

Introduce a small presentation model with a namespaced source key such as
`library:<uuid>` or `tuner:<existing-id>`, display name, optional source badge,
programme occurrences, favourite state, availability, and explicit actions.
The library adapter supplies local catalogue artwork and file source facts;
the tuner adapter keeps its current guide and measured-source behavior.

Do not unify both behind a new all-purpose session framework. Shared guide
cells and layout components receive display data and callbacks. The source's
existing controller owns tune, session lifetime, and capability enforcement.
If the redesign has already extracted this seam, adopt it instead of adding
another abstraction with the same job.

| Capability | Tuner under current plan | Library following | Detached personal title |
|---|---|---|---|
| Now/next and guide | Yes, when feed available | Yes, locally derived | Channel remains available as navigation context |
| Preview focus without tune | Yes | Yes | Yes |
| Source/playing format distinction | Yes | Yes | Yes |
| Restart from source beginning | No | Watch from start | Yes |
| Arbitrary source seek | No | No | Existing VOD controls |
| Record | No | No | No |
| Return | Return to live expands current tuner picture | Expand current picture | Return to channel resolves current occurrence |

Do not reuse identical text for actions with different consequences. In
following mode **Fullscreen** expands the picture; **Resume live** or
**Return to channel** explicitly changes time/occurrence.

### 10.2 TV focus and playback stay independent

Implement all three approved layouts. Keep the programme under focus and
the programme actually playing as separate state, with a visible Watching
marker. Directional movement updates details; Select tunes a current
occurrence once. Select on a future/past guide cell opens details only.
Watch from start is available in those details only when the file is still
authorized/available, as an explicit personal playback action.

Keep pinned channel/time headers, duration-proportional cells, remembered
focus/time anchors, reachable toolbar/Info/Layout controls, and deterministic
Back behavior from the native plan. The first directional action over hidden
controls reveals them; it cannot retune underneath the picture. Switching
layouts, guide modes, or opening details preserves the same current session.

### 10.3 Mobile creator and player are first-class

Portrait viewing uses a compact 16:9 player, On now / Guide / Favorites, and
whole-row touch targets. Portrait Guide defaults to the selected channel's
vertical schedule with channel picker and date/time context. The optional
grid, landscape/tablet layout, accessibility sizes and PiP behavior follow
the native layout plan. Rotating or changing the schedule picker does not tune.

Creation/editing uses three resumable form steps:

1. **Content:** subject preset, library scope, metadata rules, explicit title
   search/addition, exclusions, and an always-visible matching count.
2. **Playback:** order, specials, automatic refresh, first-ten preview and
   repeat duration. Explain next-rotation versus next-programme activation.
3. **Channel:** name, description, personal/shared when permitted, enabled
   state, review summary, and Create/Save.

Each step has Back/Next and field-level errors. Preserve unsaved inputs while
switching steps; confirm discard on leaving a dirty editor. Scope any local
draft to server and user and erase it on logout/account change. Do not persist
auth tokens in the form draft. Web/tablet may display the same sections with
a side preview; do not require horizontal scrolling on a phone.

Title search uses existing catalogue APIs for selection, while complete rule
preview uses the new bounded evaluator. A phone can search and add a series,
exclude one episode, inspect why a title matched, change visibility, and
review/save a schedule change without visiting the web app.

Use existing themes and native typography. At narrow widths show metadata on
another line; do not remove essential actions or shrink type to fit. Creation
on a TV is not required; its empty state says to create on phone or web and
offers Refresh without inventing a QR pairing service.

### 10.4 Empty and editing states must be usable

| State | Required presentation |
|---|---|
| No channels | Create channel on web/mobile; phone/web guidance on TV. |
| Draft with no matches | Explain missing matches, keep edit/save-draft available, disable enable/tune. |
| Preview excluded rows | Counts grouped by reason; inspect examples without a wall of diagnostics. |
| Building first generation | Building schedule with retryable error if it fails; no fake now-playing card. |
| Pending edit | Current schedule remains visible, with "Changes begin <absolute time>" and editor details. |
| Permission/conflict | Preserve form input and offer reload/compare, never silently overwrite. |
| One-title channel | Explicitly show that the same title repeats after its duration. |
| All files unavailable | Channel still has a truthful schedule and explicit unavailable states. |
| Old server | Explain that Library channels requires a server update; tuner/VOD navigation still works. |

## 11. Bound reads, work and observability

Cache generation vectors by `(channel_id, generation_id, content_digest)` in
node-local memory, with a 32 MiB/64-generation LRU limit, whichever is reached
first. The cache accelerates immutable data; current channel permission,
enabled state and effective-generation lookup remain authoritative. Evict
on deletion and never reuse actor-filtered presentation metadata across users.

Resolve is O(log n) after loading the vector. Guide generation emits only
requested occurrences and enforces pagination before allocation. Build
queries batch item/file/ancestry reads; no query per guide cell, provider
lookup per title, unbounded recursive descent, or disk stat per preview row.

List/guide polling is at most once per 30 seconds while visible, with an
immediate local boundary update and a coalesced authoritative refresh. Pause
it when the feature is hidden and no relevant foreground/PiP playback needs
it. These polls do not replace media control heartbeats. Do not persist a
viewer's current channel offset every second.

Record bounded counters/histograms for build attempts/outcomes, build queue
depth, candidate rows, publication conflicts, resolve latency, generation
cache hit/miss, occurrence-change retries, unavailable slots, tune-to-frame,
and programme-transition latency. Channel/session IDs may appear in existing
structured diagnostic logs, not as unbounded metric labels. Never log
credential URLs, absolute file paths, raw user keyword text, or full recipes.

Use existing client playback telemetry for first-frame/transition observations
and the existing server diagnostic surface for errors. There is no new
dashboard, remote analytics destination, or benchmark service in this effort.

## 12. Prove the behavior with focused evidence

Tests are development evidence, not a new pre-merge full-suite requirement.
Keep deterministic schedule/authorization fixtures small. Use existing store,
HTTP, web and native test harnesses. Physical remote/player behavior still
needs device observation; a reducer passing cannot prove D-pad reachability.

| Area | Acceptance cases |
|---|---|
| Recipe | AND across fields, OR within field, explicit include/exclude precedence, all-empty rules, explicit all-in-scope, inherited show metadata, specials, missing ancestry, Unicode matching, unknown year, limits and duplicate files. |
| Schedule | Golden vectors for both orderings; exact boundary chooses successor; cycle wrap; single title; unequal durations; restart produces identical schedule; DST/device-clock changes do not change UTC occurrences; overflow rejected. |
| Publication | Two writers race; stale claim/revision loses; process death mid-staging exposes nothing; pending-generation window crosses correctly; edits within 30 seconds choose a later boundary; empty rebuild preserves old schedule. |
| Store | Fresh/upgrade parity in SQLite and Hiqlite; import/backup/restore retains definitions/order/favourites; account deletion cannot leak into reused IDs; no wall-clock/random SQL. |
| Authorization | Cross-user personal IDs/cursors/resolve/create fail; shared edit admin-only; owner checks at commit; permission changes during start rejected; no secret/path leakage. |
| Playback | Two devices resolve the same occurrence; late start catches up; slot changes during start; multiple rapid tune requests settle only the latest; restart/return; pause beyond boundary; recovery purpose survives node change. |
| Missing media | Item deleted, file replaced under same ID, missing mount, corrupt/early EOF, duration change; clients keep shared boundaries and show truthful failure. |
| History | Following emits no watch writes, start/stop scrobbles or watched notifications; Activity/lease still works; normal same-item VOD on another device still updates history; late callbacks cannot cross modes. |
| Guide/navigation | Three TV layouts, focused versus playing identity, variable cell duration, future details, missing artwork, empty channels, toolbar and Back escape, no retune on layout/filter/picker/rotation. |
| Creator | Complete phone and web workflows, editable empty presets, explicit per-title exclusions, conflict/retry, dirty-form back, draft erase on user/server change, mobile text scaling. |
| Resources | Zero channel-owned media processes with zero viewers; queue/cache/response caps enforced; failed builders recover; no recurring Raft writes per slot/cycle. |

Retain a concise measurement record on the implementation branch naming
commit, server topology, client build, device, media codecs and network:

- Two devices on a healthy LAN follow one channel: source position differs
  by no more than 2 seconds after bounded startup correction.
- At least five transitions including a movie/episode boundary and a codec
  change: record each start-to-frame and end-to-next-frame latency and any
  black/loading interval. Target <=3 seconds for already-ready local VOD;
  cold transcodes retain existing startup budgets and must be reported as such.
- Switch all three TV layouts during playback: unchanged media session ID
  and no start/stop caused by presentation-only actions.
- Run with zero viewers for at least two programme boundaries: no new media
  opens/producers or periodic occurrence writes.
- On iPhone and Android, create, edit, exclude, save, tune, restart and return;
  observe portrait/landscape and one tablet-size layout.

These are acceptance targets for the feature, not claims already measured.
If a target cannot be met, retain the evidence and identify the specific
limitation; do not mark the feature complete by deleting the case.

## 13. Build in six reviewable work packages

Use one `effort/library-channels` integration branch. Task branches start
from its current head and target that effort. The plan does not ask Sol to
spawn agents; execute sequentially or use an explicitly authorized workflow.

### 13.1 Establish the base, compiler, and shared UI seam

**Work:** read current AGENTS, pipeline, compile loop and native layout plan;
identify the redesign's actual merged components or integration branch before
editing overlapping native surfaces. Backend work can proceed while that UI
effort completes. Record the intended integration heads, source anchors and
accepted deviations here. Do not copy unrelated working-tree changes into
this effort.

This documentation session did not change Rust. The implementation session
will: establish Rust **1.97.1** before its first Rust edit, verifying
`rustc --version`. If unavailable on the checkout host, use
[the source-only compile loop](../ci/AGENT-COMPILE-LOOP.md). Transfer committed
source with `git archive`, never `.git` or repository credentials.

**Acceptance:** baseline compilation on the pinned toolchain and a concrete
mapping from shared guide components to library adapters. If the redesign
has not merged, name the dependency and integrate its final code before
finishing native UI; do not recreate it in parallel.

### 13.2 Implement recipes, rotations and durable publication

**Work:** add pure domain/evaluator/schedule code, Store traits, both store
implementations, migrations/import parity, claim/publish CAS, cleanup, and
the refresh queue. Follow §4–§6 exactly. Add focused golden and race fixtures.

**Acceptance:** the same fixture produces the same generation/order on both
stores and after restart; racing builders cannot publish stale work;
current/pending schedules resolve correctly without periodic playback jobs.
Run affected check/Clippy/fmt before pushing.

### 13.3 Implement API, permissions and finite playback purpose

**Work:** add §7 routes and route matrix entries, per-user favourites,
resolution, finite-session channel context and owner/recovery persistence,
history suppression, Activity context and bounded errors. Add request
idempotency, counts/limits, and conflicting-write behavior.

**Acceptance:** HTTP tests exercise real store state for unauthorized IDs,
late resolves, revision conflicts and missing files. A following finite HLS
session produces no normal history/start notification; a concurrent ordinary
VOD session for the same item still does. Node handoff retains purpose.

### 13.4 Deliver the complete web feature

**Work:** add the navigation entry and creator, list/guide, management actions,
source adapter, mode-aware player orchestration and failure states. Use a
separate channel module and existing player/controller boundaries. Read
§8–§10 before wiring ordinary VOD ended/progress callbacks.

**Acceptance:** browser user journey creates and edits a channel from real
fixture media, waits across boundaries, restarts/returns, and confirms no
history mutation while following. Check narrow mobile-browser rendering,
keyboard navigation and stale-response cancellation.

### 13.5 Deliver Apple and Android viewing plus mobile authoring

**Work:** extend native API/domain models and existing player controllers,
reuse redesigned layouts on both TVs, and implement complete phone/tablet
editor steps. Apply source capabilities and history behavior across every
termination/recovery path, including system PiP and foreground resume.

**Acceptance:** all three TV layouts work on Apple TV and Google TV; physical
Select/Back can reach and leave all controls; both phone platforms complete
creation/editing without web assistance; the §12 transition/history cases
hold. Advance each shipped platform's build counter in the same work.

### 13.6 Integrate, document and promote the complete effort

**Work:** finish targeted evidence, update current behavior/API/client docs,
freeze task merges, merge current main into the effort and re-run the pinned
compile loop against that exact committed source. Include the doc index and
functionality ownership in their owning commits.

**Acceptance:** no required surface remains a placeholder or "later"; actual
limitations are stated; all review findings are owned and resolved; current
head has a green Main promotion gate before merge. Follow §14 for the exact
review/fast-lane convention.

## 14. Keep validation, versions and documentation with their changes

Add implementation paths to functionality point `library.channels` as each
file is introduced, with dependencies on catalogue, identity, playback and
affected client points. This handoff initially owns only its documentation
path; do not add globs for files that do not exist yet or weaken ownership
checks. Preserve existing ownership of shared player/store files.

Update [API.md](../API.md), [FEATURES.md](../FEATURES.md),
[ARCHITECTURE.md](../ARCHITECTURE.md), [PLAYBACK.md](../PLAYBACK.md),
[OPERATIONS.md](../OPERATIONS.md), and the affected
[Apple](../clients/APPLE-CLIENT-PARITY.md) /
[Android](../clients/ANDROID-CLIENT-PARITY.md) parity records as behavior lands.
New evidence documents belong in the appropriate subject folder and need an
index row in the same commit. Update this plan's status only when supported
by implementation/evidence, not because a PR exists.

Use the current repository command profiles, not guessed replacement gates:

```bash
rustc --version                  # Must report the repository-pinned toolchain.
cargo check -p plurxd --all-targets
cargo clippy -p plurxd --all-targets -- -D warnings
cargo fmt --all -- --check
make validation-lint              # Functionality ownership and catalogue.
make operations-check             # Documentation/current-build/static contracts.
make history-check                # Corrective commits have their owed evidence.
```

Enable the relevant existing Store feature profiles when compiling/testing
both backends, and compile affected Apple/Android targets using the current
pipeline commands. The example Rust commands do not replace affected-surface
checks or feature-profile compilation. Do not use CI to find compiler errors.

For task PRs into the effort, no main-only review or fast lane is requested.
For the completed effort's PR into **main**: open draft, request exactly one
adversarial agent review, address every finding on the draft, verify the
fixes as author, then mark ready; the fast lane starts automatically. Merge
only after the **current head** has a green **Main promotion gate**. No second
review, re-review, panel, or approval follow-up. Returning to draft stops the
ready-PR lane; there is no fast-lane label to add or remove.

Full runtime suites belong to the separately dispatched sweep. Focused
feature tests and device evidence guide implementation, but this handoff
does not add unit/integration/browser/device suites to the fast lane or make
the full sweep a PR prerequisite. Never install the disabled pre-commit hook.

A corrective commit owes regression evidence under the current history
policy unless its changed test is direct evidence. A shipped Apple or Android
change owes that platform's build-counter increment. A workspace release owes
both counters and aligned marketing versions. Finish that paperwork before
leaving draft; do not create later paperwork-only repair commits.

## 15. Sol's starting instruction

> Implement this document in order on `effort/library-channels`, coordinating
> the native UI seam with the completed Live TV redesign. Deliver the server,
> web, iOS/iPadOS, Android phone/tablet, Apple TV and Google TV scope, including
> mobile creation/editing. Establish the pinned Rust compiler loop before
> editing Rust. Reuse the existing finite-media playback and Store boundaries;
> preserve tuner behavior and ordinary VOD history. Record concrete deviations
> and evidence in this plan. Follow the repository's one-review, main-only
> fast-lane process and do not declare completion with a missing platform,
> unverified session/history boundary, or an unresolved compile error.
