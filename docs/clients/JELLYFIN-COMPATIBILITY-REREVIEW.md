# Jellyfin compatibility build contract — re-review

**Status:** re-review delivered · **Verdict:** APPROVED to start J0; fix R1
and R2 before J4 · **Written:** 2026-10-02 EDT · **Reviewed:** the revised
`JELLYFIN-COMPATIBILITY-BUILD.md` plus `JELLYFIN-COMPATIBILITY-REVIEW.md` §6,
commit `890bca0fc` on Paul's local branch `codex/playback-seek-30s`.
Source facts are checked against `f1f1390f1`.

## 1. Verdict

The disposition is faithful. Each of B1–B5, M1–M6 and the minor items
landed in the section it names. Several landed stronger than I asked:

- a transactional revision instead of a timestamp;
- commit-time fence checks;
- per-play `put_final`;
- refusal **before** a rolling allocation, instead of after it.

The architecture and the J0 plan are ready. What remains is:

- one new defect, in the mapping I proposed (R1);
- one correction to my own B2, which the revision inherited (R2);
- small precision items.

## 2. Findings

### R1. `PlaySessionId → playback_id` inverts native supersession (§6.2, D7)

Native `CreateSession.playback_id` is documented as "Stable for one player
instance. Supersession is keyed by it" (`http/hls/create.rs:39-40`). A
create under the same `playback_id` replaces its predecessor.

Jellyfin mints a fresh `PlaySessionId` for every `PlaybackInfo`. A client that
re-negotiates on an audio, subtitle or quality change therefore gets a new
`playback_id` every time. Expected sequence for Android TV, to confirm in J0:

1. It plays a transcode under play A.
2. The user switches the audio track.
3. The client negotiates play B, which creates a second session.
4. A is never superseded. It lives until a Stopped or `ActiveEncodings`
   arrives for it, which isn't guaranteed. Otherwise it waits out the 300 s
   VOD idle clock while holding its working set.

Every track switch leaks one session for up to five minutes. A06 cannot see
this, because it tests two devices.

**Amendment:**

- Derive `playback_id` from the player instance: a digest of `(user,
  DeviceId, client family)`. This is the same scope §5.1 already uses for
  token replacement.
- Put `PlaySessionId` plus the delivery digest into `request_id`.
- A second play on the same device then supersedes the first, as native
  does. Keep "several bindings per device" for identity and tombstones only,
  and state that producer concurrency is one per player instance.
- Add an A11 row: switch tracks or quality in a transcode on a client that
  sends no Stopped for the old play. Exactly one producer must remain.

### R2. A missing index blocks only copy; my B2 overstated it (§7.2)

VOD refuses for a missing index only when there is also no encoding:
`if index.is_none() && prepared.encoding.is_none()` (`vod/serve/create.rs:394`).
**An encoded VOD transcode starts without a fragment index.**

My original B2 said every passive transcode on an unindexed title dies. That
was wrong. The real fallback set is this (`transcode/live_recovery.rs:43-51`):

| Refusal | What it means | What the façade should do instead |
|---|---|---|
| `vod_index_pending` | HLS **copy** of an unindexed file | Progressive remux if the chosen profile accepts that container. Otherwise encoded VOD if `EnableTranscoding`. Don't wait for it. |
| `vod_source_unsupported` | No probed duration, or parameter sets vary mid-film | Encoded VOD if permitted. Otherwise an honest refusal at `PlaybackInfo`. |
| `vod_transcode_unavailable` / `vod_subtitle_burn_unavailable` | No encoded recipe can be resolved | An honest refusal |

**Amendment:**

1. **Correct the "source needing video transcoding" sentence in §7.2.**
   Transcode is the route that does **not** depend on the index.
2. **Stop spending the 15 s wait on `vod_index_pending`.** An index build
   takes minutes, so waiting for one is pure delay. Enqueue or promote the
   preparation and take the next candidate immediately. Keep the bounded
   wait for capacity, owner-transition and startup-timeout codes, which can
   clear in seconds.
3. **Decide the predictable outcomes in `PlaybackInfo`, not at the master
   GET.** Index readiness and probed duration are known at negotiation.
   Advertise only deliveries that can start. A `TranscodingUrl` that is
   certain to 504 after 15 s is the worst outcome on the list.
4. **Retitle J0's "unindexed source" case** as "unindexed copy request" and
   "unindexed transcode request". Expect different results.

### R3. Deletion hooks cannot see FK cascades (§5.2)

`items` and `files` are deleted by `ON DELETE CASCADE` from `libraries`,
`items(parent_id)` and `items` (`store/sqlite/mod.rs:93-118`, with hiqlite
mirroring them). Application code never sees the cascaded rows, so "native
deletion/cascade paths must retire mappings in the same transaction" can't be
met by editing call sites.

**Amendment:**

- **Use triggers.** Retire mappings with `AFTER DELETE` triggers on `users`,
  `libraries`, `items` and `files`. The store already uses triggers
  (`fragindex.rs:1320`, `dv_conversion.rs`, the `background_jobs` SQL), and a
  trigger fires inside the deleting transaction on both backends.
- **Keep the trigger body deterministic**, because hiqlite replays statements
  on every replica. Set `retired = 1` or record `OLD.id`; never use
  `unixepoch()` in the trigger. If a retirement time is needed, stamp it
  later from a parameterized statement.
- **J1 test:** delete a library and assert that every descendant item and
  file mapping is retired on both backends.

### R4. Bump the revision inside the Store, not at callers (§7.4)

The production manual-edit writers are `watch.rs:128,151` and
`plex.rs:383,396`, all through `Store::set_watched_tree`. `Store::set_watched`
is also part of the trait. Put the revision bump inside those Store methods,
on both backends. A future caller then can't bypass the fence.

### R5. The fence silences a client's own edits (§7.4)

As written, any manual edit after a binding is created discards that play's
later progress. Two cases break:

- **Unwatch mid-play.** A user unwatches mid-play from the same app, then keeps
  watching. Every later beat and the final stop are discarded.
- **Mark played, then stop.** Infuse marks an item played near the end via
  `PlayedItems`, then sends Stopped. Probably harmless, but that is luck,
  not design.

**Amendment:** an edit made under the same token and device advances that
binding's snapshot. Only edits from elsewhere fence it. Add both cases to A07.

### Minor

- **R6 (§5.1, anonymous artwork).** Artwork resizes happen on demand: a resize
  child runs on a derivative miss (`http/images.rs:391, 565, 772`). Without
  bounds, anonymous GETs with arbitrary sizes become unauthenticated CPU work.
  Make "bounded image transforms" concrete:
  - quantize to a fixed width set;
  - serve existing derivatives only;
  - share the existing resize admission;
  - cap misses per address.
- **R7 (retained review §2).** Line 100 names a node (now `lab6`) and a media title
  (now “reference film”), which breaks the public-mirror naming rule (lab node aliases and
  reference-film labels). It also collides visually with finding label
  M6. Sanitize when porting the docs to main.
- **R8 (where the docs live).** All three docs exist only on an unpushed local
  branch that also carries unrelated work, so no other session can find them.
  The disposition's choice not to rebase is fine for the implementation base.
  The docs themselves should still land as their own draft docs PR from
  `main`, so the contract is discoverable before J0 starts.

## 3. Changed D answers

| | Re-review answer |
|---|---|
| D5 | VOD passive is confirmed. The rolling fallback is excluded before allocation. Unindexed **copy** goes to remux or encode; unindexed **transcode** works (R2). |
| D7 | `playback_id` = player instance; `PlaySessionId` and the delivery digest go into `request_id` (R1). |
| D3 | Incarnation plus `AFTER DELETE` triggers (R3). |
| D8 | Store-level revision bump, with an own-edit exemption (R4, R5). |
| D12 | Unchanged: J0 4–6 days, release 5–9 engineer-weeks. R2 removes work: no wait-for-index path to build. |

The other D answers stand as revised.

## 4. Order

1. Land the docs as a draft docs PR from `main`, with R1–R8 folded in and
   R7 sanitized.
2. Run J0. The receipts now include: per-client renegotiation behaviour on a
   track switch (R1), and unindexed copy versus transcode outcomes (R2).
3. Continue J1 onwards as revised.

## 5. Author disposition — R1–R8

**Reconciled:** 2026-10-02 · **Publication base:** `9a719fcb7` ·
**Source reviewed:** `f1f1390f1`

Sections 1–4 retain the supplied re-review, with R7's node/title names
sanitized. Its original attachment SHA-256 is `739d69a057405a4d405610d41c628af975a6501ef9f5e1eccdfe6d5781c6a969`.
The original attachments remain outside the repository. This section records
amendments to the [current build contract](JELLYFIN-COMPATIBILITY-BUILD.md),
which is authoritative where the historical reviews differ.

| Finding | Disposition | Build amendment and required evidence |
|---|---|---|
| R1 | Accepted | §6.2/§7.1/D7 use a stable authenticated player digest for playback_id and PlaySessionId plus delivery digest for request_id. Multiple bindings retain identities, not concurrent producers. J0/A11 require track/quality renegotiation without old Stopped and exactly one producer; A06 preserves independent devices. |
| R2 | Accepted with a source correction | §7.2 separates copy's index dependency from encoded VOD. No wait-for-index path; known failures are resolved at PlaybackInfo. Encoding bypasses index and copied parameter-set constraints, but **does not bypass missing positive duration**: the duration guard in vod/serve/create.rs applies before either plan. J0/A10 test copy and transcode separately plus missing duration. Only transient startup/capacity/owner outcomes use the bounded wait. |
| R3 | Accepted | §5.2 specifies deterministic AFTER DELETE triggers on all four entity tables, with retired=1 rather than a replica-local timestamp. J1/A18 delete a library and verify every descendant mapping is retired on both backends, including row reuse. |
| R4 | Accepted | §7.4 puts each affected item's revision bump inside both Store manual-write methods on both backends, covering native, Plex and future callers. |
| R5 | Accepted with race qualification | §7.4/A07 advance only the eligible active same-token/device binding atomically with its own edit. Already externally fenced bindings cannot revive; queued old beats keep their old revision. Unwatch-then-progress and mark-played-then-Stopped continue. Indistinguishable delayed packets remain best effort. |
| R6 | Accepted | §5.1 quantizes to 300/500/780, serves existing derivatives anonymously, returns a miss without work, shares existing admission for authenticated/background materialization and bounds per-address miss budgets. J2 measures misses/saturation; J0 verifies artwork consequences. |
| R7 | Applied | Node/title references in both retained reviews are sanitized. Finding M6 remains unchanged. Original-attachment hashes are provenance, not claims that these sanitized files are byte-identical. |
| R8 | Applied to publication workflow | The four indexed documents form a separate docs-only branch from current main for a draft PR, before J0. No unrelated branch commits, raw captures, scripts or implementation are included. |

The author checked the duration/index guards, native stable-player contract,
FK cascades, Store watch methods and image derivatives/admission against the
review source. The VOD create, HLS create, image handler and SQLite watch files
are unchanged between that source and publication base `9a719fcb7`. This is
source verification, not compiler qualification of a future implementation or
physical-client acceptance. Documentation checks and the normal commit hook
are recorded in the draft PR. J0 remains unrun; its estimate stays 4–6 days,
and the useful release stays 5–9 engineer-weeks cumulative.
