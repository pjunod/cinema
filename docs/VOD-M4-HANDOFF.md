# VOD M4 — web adoption and release acceptance

> **Superseded rollout boundary — 2026-08-25.** M4's default-off and safe-live-
> fallback language no longer describes production. The accepted cutover is
> [VOD-CUTOVER.md](VOD-CUTOVER.md): HLS creates are VOD-only, ineligible media
> fails typed, and disabling VOD refuses HLS instead of restoring live HLS.

**Status:** MERGED + DEPLOYED 2026-08-25 — feature tip `50380267`, merge
`3d6f492d`, fleet build `v0.2.7-1551-g3d6f492d`; physical evidence remains
below
**Executes:** [VOD-PRESENTATION-PLAN.md](VOD-PRESENTATION-PLAN.md) §8, M4
**Predecessor:** [VOD-M3-HANDOFF.md](VOD-M3-HANDOFF.md)
**Operator workflow:** [PLAYBACK-TESTING.md](PLAYBACK-TESTING.md#film-addressed-vod--enable-it-and-prove-the-client-contract)

M4 made the shipped web player a VOD-aware client. The subsequent cutover made
that presentation mandatory across server and clients. Transcode-rung VOD
remains gated and now fails explicitly instead of changing presentation.

## What changed

### One client contract owns every HLS open

Every web session-opening path passes through `openSession`, which overwrites
the request with:

```json
{
  "presentation": "vod",
  "block_budget_secs": 8
}
```

The same `vodClientContract` configures hls.js with a 10,000 ms first-byte
deadline. The server budget therefore settles two seconds before the browser
aborts. Timeout retries are explicit, and error retries continue past the
30-second producer watchdog so a producer that cannot make bytes ends in a
typed server failure instead of an unclassified `fragLoadError`.

The server treats an absent `playback.vod_presentation` setting as enabled. An
explicit off value or an ineligible file returns a typed refusal.

### VOD is visible as VOD

`GET /api/v1/hls/{session}/status` now has a VOD-shaped response for VOD
sessions. It reports the producer state, fetched and materialized frontiers,
planned/materialized bytes, node working-set use and budget, admission, hold,
failure, and completion. A status read does not touch the media lease.

The web playback panel labels the method `Remux · VOD` and describes Waiting,
Materializing, Holding, Complete, or Failed. It does not use a cache label for
a working-set-only rendition.

VOD handle attachment and every terminal cause also emit the existing
`session_start`/`session_end` playback events with `method=remux`,
`encoder=vod`, and the established terminal reasons. Idempotent repeated ends
do not duplicate history, and Prometheus gives `vod` its own encoder label.

### The producer gets one final deadline

`playback.vod_materialize_budget_secs` defaults to 30 seconds. Its clock starts
on an init or segment's first blocked demand and survives the shorter 8-second
HTTP waits and retries. Materialization clears it. Expiry terminates the
generation and wakes every waiter with `producer_failed`.

### Native text subtitles follow the immutable plan

A VOD copy session can now serve the existing native-subtitle multivariant
playlist. The video child is the immutable VOD playlist, the WebVTT child
mirrors its `MEDIA-SEQUENCE`, segment durations, `PLAYLIST-TYPE:VOD`, and
`ENDLIST`, and each VTT segment uses the plan entry's film-time window.
Bitmap and styled subtitle burns return `vod_subtitle_burn_unavailable` until
the planned VOD transcode producer exists.

### Operators can provision and enable it

Settings → Playback exposes:

- the VOD presentation opt-in;
- the node-local fragment-index interval;
- the node-wide working-set budget; and
- the producer deadline.

The web client always declares the capability. The server setting is a
maintenance kill switch and defaults on; it is not a presentation rollback.
The index interval defaults to 15 minutes. An unindexed title reports
`vod_index_pending` until its index exists.

## Executable browser contract

Run:

```bash
scripts/playback-lab run --suite vod --browser chrome \
  --json out/vod.json
```

The suite owns an isolated database, scratch directory, library, and server. It
enables VOD, waits for the fragment-index pass, and drives the shipped web page
through three reviewable cases:

| Case | Hard assertions |
|---|---|
| Steady | VOD at first and last sample; one player generation; zero keeper fires; one server attach |
| Seek storm | 20/20 seeks land within 0.5 s; same session and generation; zero keeper fires; one server attach |
| Suspend/resume | eight-second media pause resumes; same session and generation; zero keeper fires; one server attach |

The suspend case is a deterministic browser surrogate, not evidence that a
machine slept. The physical acceptance below retains that distinction.

## Release gates

Do not use `cargo test --workspace` as a Mac gate. The recorded macOS
TMPDIR/directory-capability artifact makes 28 unrelated
`fs_secure`/metadata/transcode-manifest tests fail on plain main. The focused
Rust tests, web policy tests, playback-lab contracts, repository make gates,
and Linux CI are the relevant automated evidence.

Before transport:

```bash
cargo fmt --all -- --check
cargo check -p plurxd
cargo test -p plurxd status_describes_vod_without_touching_its_idle_lease
cargo test -p plurxd materialize_watchdog_spans_http_retries_and_fails_typed
cargo test -p plurxd subtitle_playlist_and_vtt_mirror_video_segments_at_resume_timeline
node tests/playback/web-policy.test.js
node tests/playback/network-shaping.test.js
scripts/playback-lab run --suite vod --browser chrome --json out/vod.json
scripts/playback-lab run --suite stall-recovery --browser chrome \
  --network-profile 8mbps-to-1.5mbps --json out/stall-recovery.json
```

The self-hosted Linux runners shorten compilation and unit-test time. They do
not replace the following nynuc acceptance, because that proof names a real
library title, NAS path, browser, and host sleep state.

## Nynuc acceptance protocol

Run all three against the exact deployed commit and retain browser evidence plus
`/api/v1/system/logs?level=trace` for each case.

1. **Two-hour 4K remux.** Start a current indexed 4K remux in the web player and
   watch it end to end. Pass only if the client reports VOD throughout,
   `_keeperFires` stays zero, the player generation and session id do not
   change, the log has exactly one `vod session attached`, and no media error or
   stall occurs.
2. **Twenty-seek storm.** In one session, seek to twenty non-linear film-time
   positions and wait for each to land. Pass only if all twenty land natively,
   the player generation and session id stay fixed, `_keeperFires` stays zero,
   and the log still has exactly one `vod session attached`.
3. **Real sleep/wake.** During the middle of the same class of title, put the
   browser host to sleep long enough to cross an ordinary live-playlist refresh
   interval, wake it, and resume. Pass only if the same session continues with
   no media error and no keeper fire or replacement attach.

Record the deployed full SHA, file id/title/duration, browser version, VOD
session id, player generation, keeper count, attach count, landed seeks,
start/end times, and artifact paths in the evidence table below.

| Evidence | Result |
|---|---|
| Automated VOD suite | **PASS** — 3/3 on `50380267`: steady, 20 seeks, suspend/resume; [runner job](https://github.com/pjunod/plurx/actions/runs/32871768220/job/97880727670) |
| Automated stall-recovery suite | **PASS** — 1/1 on `50380267` with the 60 s evidence window; same runner job |
| Linux self-hosted CI | **PASS, executable gates** — functionality 12/12, storage contracts, both release compiles, lint, layout, Android, and container smoke passed. Hosted-Apple billing and exhausted artifact storage prevented wrapper checks from turning green; [exact exceptions](https://github.com/pjunod/plurx/pull/563#issuecomment-5413811009). |
| Fleet server deploy | **PASS** — Ansible deployed merge `3d6f492d`; nynuc and nuc3 rebuilt and health-checked, m6 and nuc4 were already on the exact stamped build; zero failed or unreachable tasks |
| Nynuc 2 h 4K | Pending |
| Nynuc 20 seeks | Pending |
| Nynuc sleep/wake | Pending |

## Deliberate boundary

Transcode-rung VOD remains gated on P2/D6. The hls.js half supports nominal
EXTINF timing, but AVPlayer and Media3 have not yet supplied the device half of
that measurement. Until they do, a transcode rung returns
`vod_transcode_unavailable`. Durable 410 semantics across a node restart remain
separately covered by M3's route resurrection and tombstone tests.
