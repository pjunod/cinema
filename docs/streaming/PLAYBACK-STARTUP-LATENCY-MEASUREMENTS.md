# Playback startup measurements — separate readiness from continuity

**Status:** M0/M1 investigation in progress; M2 policy unqualified ·
**Written:** 2026-09-29 EDT · **Source:** diagnostics `b3ca3a0de`; current main integrated at `af60b278c`; serving-settings correction in draft PR #627.

Companion to [the build contract](PLAYBACK-STARTUP-LATENCY-BUILD.md) — this
is retained evidence and its limits, not a proposed shipping threshold.

## Monotonic diagnostics — each elapsed value has one origin

The implementation adds bounded phase names to existing structured logs.
Session identity is hashed by the existing log helper. File/attempt identities
are log fields, never metric labels. The phases do not alter publication,
source validation, actor admission, scratch ownership or startup deadlines.

| Phase | Origin and endpoint | Interpretation |
|---|---|---|
| `vod_create` | Start/end of the existing immutable-VOD create attempt | Includes exact recipe/index lookup and preparation admission; success or refusal |
| `create` | After request claiming to completed route creation | Excludes decision and the earlier claim wait; includes VOD refusal plus rolling creation when used |
| `copy_writer_first_complete_segment` | Copy writer construction to first successful segment rename | A completed object, not permission to serve it |
| `copy_writer_gate_open` | Same writer origin to successful first non-final playlist write | The writer's 12-second gate; separate from server publication |
| `writer_inventory_to_first_snapshot` | First actual observation of a gated writer playlist to actor-admitted snapshot availability | Includes publication wait and admission work; the worker poll may delay the initial observation |
| `copy_init_validation` | Validation entry to result | Separates time spent validating from the timestamp of its success log |
| `first_response_admission` | Response authorization entry to the first admitted producer-media response | Exact owner and actor fences remain authoritative |

Failures or cancellation before a completion event leave that phase absent;
absence is not a zero-duration success. Browser click-to-frame remains the
browser's monotonic duration. Do not subtract it from server wall timestamps.
The short-source ENDLIST path retains its existing completion contract;
`copy_writer_gate_open` currently records non-final gate opening only.

## Incident replay — the sixth endpoint crosses the existing gate

`scripts/playback-startup-trace` replays the sanitized six durations and
7.965-second achieved-origin lead. It performs design arithmetic; it is not
a daemon or browser execution. The authored daemon reproduction is
`rolling_publication_budget_incident_resume_waits_for_sixth_endpoint` in
[chunk_03.rs](../../crates/plurxd/src/transcode/tests/chunk_03.rs).
It is **authored, not run**, per the build's deferred regression sequence.

| Candidate usable runway at 1× | Required endpoint | First incident endpoint crossing it |
|---|---|---|
| 12 s | 19.965 s | Segment 2, 23.231 s |
| 16 s | 23.965 s | Segment 3, 30.989 s |
| 24 s | 31.965 s | Segment 4, 39.664 s |
| 32 s | 39.965 s | Segment 5, 50.967 s |
| 48 s | 55.965 s | Segment 6, 58.558 s |

The recorded first/sixth completion timestamps differ by 5.990 seconds.
Intermediate completion timestamps are not supplied or interpolated.
The capacity prefilter covers the clamp edges and 0.25×–4× rates with
0.95×/1.05×/1.2×/2× production relative to consumption. Its transfer/append
and scheduling allowance is an explicit 2.5-second experiment assumption,
not a measured production bound. No candidate is qualified by that output.

## Exact preparation — contention and a different pipeline are observed

Read-only observations on `nynuc` at Unix milliseconds `1790735954244`,
`1790736051977`, `1790736082565` and `1790736212763` show:

- File 6456 has no local fragment index or terminal preparation outcome.
- Its exact fragment job remains queued at priority 3, with zero failed
  attempts and an elapsed age rising from 2303 to approximately 2561 seconds.
- Its requested pipeline digest begins `9e7e13ea`; the available peer
  artifact's pipeline begins `6386de24`. Their attested source digest is the
  same, but the artifact cannot satisfy a different exact pipeline.
- Both live `source_io` reservations are occupied by priority-0 subtitle
  extraction jobs. At the final observation their job ages exceed 258,000
  seconds; job creation age does not prove uninterrupted claim age.
- Discovery cursors and timestamps advance on three nodes during the window.
  A progressing discovery cursor is not proof that the exact build can claim
  the occupied shared resources.

This classifies the observed absence as queued foreground work under shared
source-I/O contention, with an incompatible remote artifact. It does not
prove permanent starvation or the cause of every client's slow startup.
[Safari seek PR #623](http://192.168.4.7:3000/noirr/plurx/pulls/623) already
implements durable viewer interest and a reserved source-reader lane.
Integrate its final main result before duplicating that admission repair;
do not steal its live claims or reset production queues.

## Generated transport experiment — continuity measurement is still running

`scripts/playback-startup-lab` serves generated H.264/AAC fMP4 with a fixed
16-second target. It uses the repository's shipped hls.js library or native
Safari's video element, without an accelerated client reload loop. Its
completed inventory grows at the specified production ratio; segment-bearing
snapshots are chosen on 8-second ticks and retain a 160-second window.
The initial burst is explicitly modeled at 8× with a 3-second setup cost.
It does not measure actual daemon source production, actor fences or scratch.

The first ongoing comparison uses 32 seconds of post-position runway and
7.965 seconds of origin lead, so complete-segment rounding exposes 48
seconds total and 40.035 seconds after the requested position. At 1.05×,
Chrome hls.js reported first frame at 9.126 seconds and continued without a
reported stall through a captured 23-minute window. Safari native
reloads approximately every 16 seconds and shows moving generated video,
but its first-frame callback remains absent; TTFF is **unavailable**.
The original native observer counted `waiting` only after the missing frame
callback, so its zero counter is not valid stall evidence. The tracked lab
now counts waiting after sampled playback progress independently of TTFF.
The native window was subsequently closed, so that run is incomplete.
These results do not select a shipping policy or prove a 30-minute run.

Bulky generated media and evolving receipts are owned by this effort under
`/private/tmp/plurx-startup-evidence-20260929`. Retain a final sanitized
summary and hashes before cleanup. The Mac computer-use tool briefly reported
that the machine was locked; retry succeeded. This was an automation error,
not a confirmed physical-device blocker.

## Preparation correction — one consistent admission-settings read

`TranscodeManager::vod_settings` previously issued six serial `get_setting`
requests on every VOD attempt, including an index miss followed by rolling
fallback. The Hiqlite backend makes each a separate consistent read. The
correction uses the existing bounded `get_settings` contract to read exactly
those six keys from one snapshot. That removes five Store round trips without
caching settings or changing their defaults, clamping, request caps, or explicit
maintenance refusal. SQLite also reads the tuple in one statement.

`vod_settings_snapshot_preserves_budgets_and_maintenance_refusal` is authored,
not run. It covers stored values, invalid values, server/request budget caps,
and the explicit refusal. The existing blocked-GET-cap regression remains.
Pinned all-target compilation passed for the correction. No measured TTFF
improvement is claimed until the changed daemon is measured.

## Physical-device discovery — addresses are known

Xcode discovered physical Apple TV **Bedroom**, AppleTV14,1, device
`00008110-001C19140299801E`. The effort built a signed debug client and installed
it successfully. The existing device playback harness then failed at launch:
`System is asleep - foreground app launch forbidden`. It did not produce a
playback receipt. The TV must be awake before that launch can succeed.

DNS-SD discovered Google TV Streamer **Bedroom TV** at `192.168.4.108`.
The installed Android SDK reports no connected ADB devices or advertised ADB
mDNS services; connecting its usual port 5555 failed. Cast discovery proves
network presence, not a debugging connection. Wireless/network debugging or an
existing alternative ADB port is required for automated client measurements.

## Decisions — preserve contracts while evidence is incomplete

1. **Keep the current runtime runway during M0.** Earlier readiness does not
   prove that the next segment is discoverable before its buffer drains.
2. **Reuse the exact-preparation repair being qualified in #623.** It overlaps
   the demonstrated source-reader bottleneck; a second scheduler change
   would create competing ownership and integration work.
3. **Keep unavailable first-frame evidence explicit.** A moving time counter,
   decoded frame or `playing` event does not replace presentation timing.
4. **Defer unit execution.** Compiler, format and Clippy checks run during
   development; the authored replay executes only after the final review.
