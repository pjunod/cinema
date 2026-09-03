# VOD-only HLS cutover

**Status:** implemented and automated acceptance passed 2026-08-25

**Decision:** the growing live-HLS presentation is not a fallback and is no
longer available to production session creation

**Scope:** server, web, Apple, and Android HLS session contracts

**Canonical design:**
[VOD-PRESENTATION-PLAN.md](VOD-PRESENTATION-PLAN.md)

## Summary

Every HLS session is now one immutable, film-addressed VOD presentation. A
request either receives `vod:true` or a typed refusal explaining the missing
prerequisite. It never starts the former EVENT/sliding playlist engine.

Progressive direct play and progressive remux are unchanged. The cutover
applies when a playback decision needs HLS.

This deliberately replaces the staged compatibility plan that kept live HLS
through M7. The old engine was not a viable recovery path: choosing it made a
failed VOD prerequisite look like playable degradation while restoring the
session churn, mutable playlists, live-edge behavior, and recovery loops VOD
exists to remove.

## Wire contract

- `presentation:"vod"` is accepted. Omitting `presentation` also means VOD so
  an older client cannot enter live HLS by accident.
- Any explicit value other than `vod` returns HTTP 410 with
  `code:"live_presentation_removed"`.
- A successful HLS create always returns `vod:true`.
- Web, Apple, and Android send `presentation:"vod"` and refuse a response that
  does not acknowledge VOD.
- Cluster worker requests must name VOD, and ingress rejects a worker response
  with `vod:false` before publishing its route.

The former live-start implementation is excluded from production builds. Its
source and regression tests remain test-only so historical regression anchors
continue to resolve without rewriting Git history.

## Eligibility and refusals

The cutover does not pretend unfinished VOD media classes work. It makes their
status explicit:

| Request or source | Result |
|---|---|
| Copy/remux, positive duration, current fragment index, stable parameter sets | Immutable VOD session |
| Index not built yet | 503 `vod_index_pending` |
| VOD session creation administratively disabled | 503 `vod_disabled` |
| Video transcode rung | 501 `vod_transcode_unavailable` |
| Bitmap or styled subtitle burn | 501 `vod_subtitle_burn_unavailable` |
| Missing duration, varying parameter sets, or empty plan | 422 `vod_source_unsupported` |
| Owner takeover that needs a new immutable handle | 409 `vod_reopen_required` |
| The owning node is gone and a successor may still claim the session | 503 `media_owner_transition`, carrying `film_position_ms` |
| The owning node is gone and the session can never be taken over | 410 `media_owner_lost`, carrying `film_position_ms` |

The last two are answers about a session that already exists, not refusals of a
create, and they are decided from the durable route rather than from the
request: `media_owner_transition` says a successor may still arrive and the
client may keep waiting, `media_owner_lost` says none can. Both carry where to
reopen and both set `continuous:false`; only the second sets
`reopen_required:true`. `vod_reopen_required` is the different, adjacent case —
a takeover *did* happen and produced a new immutable handle, so there is
something to reopen onto. See
[PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md) §"§10.3's third bullet
is built".

Those refusals are honest product boundaries, not invitations to use the old
engine. Transcode-rung VOD still depends on the P2/D6 AVPlayer and Media3
timing measurement in
[VOD-PRESENTATION-PLAN.md](VOD-PRESENTATION-PLAN.md). Subtitle-burn VOD needs a
planned transcode producer after that decision.

## Operator controls

New and existing installations default to accepting VOD session creation. An
absent `playback.vod_presentation` setting means enabled; an explicit `0`
disables new HLS sessions and returns `vod_disabled`. It does not restore live
HLS.

The fragment-index job defaults to a 15-minute interval when its setting is
absent. An explicit zero pauses indexing. Settings → Playback describes both
controls with these semantics.

The equivalent administrative settings body is:

```json
{
  "vod_presentation": true,
  "vod_index_mins": 15,
  "vod_working_set_bytes": "8589934592",
  "vod_block_budget_secs": "8",
  "vod_materialize_budget_secs": "30"
}
```

## Verification

Automated cutover checks cover:

- public omission and explicit-live rejection;
- typed VOD ineligibility responses;
- private worker request and response fencing;
- immutable web fetch policy with no playlist reload keeper;
- persistent web presentation of a session-create refusal;
- explicit Apple and Android request fields plus `vod:true` response checks;
- production builds with no live-start symbols reachable.

The Chrome VOD playback lab passed all three shipped-player cases on
2026-08-25: steady playback, the native seek storm, and the suspend/resume
surrogate. All three used immutable copy-HLS, and all three reported zero
stalls. The same run caught and pinned a first-boot scheduling race: an empty
pre-library index pass no longer consumes the 15-minute cadence before the
first scan has published media.

Run the focused acceptance set from the repository root:

```bash
cargo clippy -p plurxd --all-targets -- -D warnings
node --test tests/playback/web-policy.test.js
make apple-test
make android-test
scripts/playback-lab run --suite vod --browser chrome --json out/vod.json
```

Do not use `cargo test --workspace` as a Mac gate. The recorded macOS
TMPDIR/directory-capability artifact fails 28 unrelated tests on plain `main`.
Linux repository gates remain the full Rust authority.

## Remaining physical evidence

The M4 nynuc checks remain physical evidence, not CI work:

1. one two-hour indexed 4K remux from beginning to end;
2. twenty non-linear seeks in one session; and
3. real host sleep/wake in the middle of the same class of title.

Record the exact deployed SHA, media identity, player version, session id,
keeper count, attach count, seek results, and log artifact paths in
[VOD-M4-HANDOFF.md](VOD-M4-HANDOFF.md). These checks validate the VOD route;
failure does not authorize restoring live HLS.
