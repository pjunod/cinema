# Android shaped-network measurement — acquire D3 without inventing an oracle

**Status:** open — manual protocol prepared; no physical run or D3 acceptance
claimed · **Executes:** A-04 design §5.3 and §7.5 · **Written:** 2026-09-30
against effort `225f3742a08cd3a70bef0686f3389a997441dde5`.

Companion to [NATIVE-ADAPTIVE-QUALITY-DESIGN.md](NATIVE-ADAPTIVE-QUALITY-DESIGN.md)
(the six-metric contract) and [PLAYBACK-TESTING.md](../PLAYBACK-TESTING.md)
(the existing shaper). This is the operational Android acquisition protocol,
not a new controller, harness, rollout or claim that the measurements exist.
Read §§2–3 before requesting a device session: current diagnostics cannot
produce every required metric. Preserve missing observations as `null` with a
reason; neither an absent event nor unimplemented native ABR proves zero.

## 1. Matrix and authority — sixteen separate physical runs

Use each named client, recording its exact model, OS, app and display/sink.
A different television, emulator or browser cannot substitute for one row.

| Physical client | Baseline, each profile | Dolby Vision second pass, each profile |
|---|---|---|
| Lenovo Android TV | `8mbps-to-1.5mbps`; `8mbps-to-1.1mbps-to-350kbps@12` | Same two profiles on an identified Dolby Vision title |
| Google TV | Same two profiles | Same two profiles on the same Dolby Vision title |
| Shield | Same two profiles | Same two profiles on the same Dolby Vision title |
| Android phone | Same two profiles | Same two profiles on the same Dolby Vision title |

Choose one finite baseline title/file and a fixed start position with enough
remaining duration for 180 seconds of observation. Record source codec,
dimensions, dynamic range, duration, audio/subtitle choices and file identity.
The Dolby Vision pass specifically answers whether the physical picture leaves
HDR and when; a device that cannot establish HDR at the start is unavailable
for that evidence, not a zero-transition success. Keep its failure receipt.

An operator must explicitly approve the named device, time slot, test traffic,
temporary server-origin/login changes, capture method and restoration before
execution. This document does not authorize unlocking, installing, deploying,
changing a shared server setting, or taking over another playback. Record the
before values and the precise changes approved. Developer readiness is
advisory: this measurement preflight never disables a feature switch, rejects
Save or overrides the saved choice. Baselines use the identified main build;
later A-05 candidate comparisons follow the build plan's separate authority.

## 2. Preflight — identify the build and prove acquisition is available

Before and after every run retain the server checkout SHA and dirty state,
running binary/image digest and runtime build identity, app versionCode/name,
installed APK/split hashes and signing identity from available owner receipts,
and their mapping to source. A source checkout alone is not installed-app or
running-binary evidence. Record the physical device identifier privately,
selected file/item, media hash or existing immutable media identity, start
position, server instance, selected quality (`Auto`, not Original/manual),
delivery/session/attempt identities, and all capture-tool versions. If these
cannot be independently attributed, record unknown and withhold acceptance.
Do not build or install a replacement merely to fill an identity field.

Re-verify the following symbols against the actual build rather than assuming
the September 30 source reading describes a future binary:

- [playback-lab](../../scripts/playback-lab): `deviceProxyCommand`,
  `ShapingProxy.handleControl`, `captureClientLog`, `telemetrySnapshot`,
  `isMediaPath` and `normalizeTrace`.
- [PlaybackTelemetry.kt](../../clients/android/app/src/main/java/tv/plurx/app/player/PlaybackTelemetry.kt):
  `ControllerPlaybackTelemetry.firstFrame`, `sampleStall`, `report` and
  `postPlaybackClientLog`. `ttff` supplies attempt/reason/elapsed `ms`;
  recovered buffering `stall` events are summaries, not a stationary-time
  integral. Sending is authenticated and best effort, with errors swallowed.
- [Controller.kt](../../clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt):
  `onRenderedFirstFrame`, `logQualitySwitch`, `startStatusPolling` and
  `playbackTelemetry`. `quality_switch` describes a **viewer** change;
  telemetry's `playbackRequested` reads `player.playWhenReady`, not an
  exported continuous viewer-intent timeline.
- [PlayerScreen.kt](../../clients/android/app/src/main/java/tv/plurx/app/player/PlayerScreen.kt):
  `PlaybackInfoOverlay` shows current position, presentation/status ages,
  target height, session, build and source/delivered/rendered range summary.
  Screenshots are point observations. Its stall count is not elapsed stalled
  seconds, and its rate labels are not final-60-second media-byte accounting.

Name and prove an acquisition source for every row below **before claiming a
complete trace**. An approved external capture may support observation, but a
film image or rounded position label alone does not expose exact player clock
and viewer intent. No existing Android equivalent of Apple's periodic
acceptance probes is promised here. If a missing instrument requires new app
code, record that prerequisite for its owner; this protocol adds none.
An exploratory run may retain available fields with explicit missing reasons,
but it must remain incomplete. Decide that scope before spending device time.

## 3. Six metrics — definitions and the raw observations each requires

All times below are wall seconds on an aligned timeline, not film seconds.
Each metric needs raw artifact references, coverage interval, units, acquisition
method and uncertainty. Keep event boundaries across session replacements.

| Metric | Calculation and units | Raw acquisition and current limitation |
|---|---|---|
| Stalled seconds | Integral of wall time during which the presentation clock is stationary **and viewer wants playback**; seconds. Keep startup separately and disclose the measured post-first-frame interval. Exclude viewer pause, seek discontinuity and non-presenting background time by observed intent/lifecycle, not by guess. | Requires continuous clock/intent evidence with a documented resolution and interval boundaries. Current `stall.ms` only describes recovered buffering; terminal/unrecovered gaps and non-buffering stationary presentation can be absent. No complete integral is currently exported. If that acquisition is unavailable, `null`, not sum-of-stalls or zero. |
| Switches | Count of automatic rung changes per run, with old/new rung, cause, session/attempt and wall timestamp; keep viewer changes and same-rung reopens separately. | Retain proxy client events plus Settings → Logs entries and observer intent record. `quality_switch` alone means viewer change. No native controller today is context, not evidence for a count of zero. Missing origin/rung transitions or truncated events mean `null`. |
| First-frame gap | For each switch/reopen, next presented frame time minus the initiating switch/reopen time; seconds, including unresolved open gaps. | Matched `ttff` attempt/reason/`ms` supports a reported attempt-to-first-frame duration when its start is the required boundary; it is not a continuous frame trace or automatically a viewer-action boundary. Require matched initiator and actual output evidence. Missing first frame is censored/missing, never 0; preserve observed lower bound through stop. |
| Quality regained | For **each cliff**, first presented frame on a demonstrated sustainable rung minus the actual proxy cliff time; seconds. | Requires aligned frame, rung, and ladder `total_kbps`, plus evidence of sustained delivery/presentation at that rung. A target height/status response is not a frame. Predeclare the sustainability proof and retain its whole interval; do not invent a new threshold or treat the cap as observed throughput. Existing sparse events cannot guarantee this oracle. Missing frame/rung/sustainability proof yields `null`. |
| Unexpected SDR transitions | Count of observed HDR→SDR output-grade changes not requested by the viewer; timestamp and before/after grades for each. | Capture source/delivered/rendered facts and a verified physical sink HDR indication throughout the DV pass, with its acquisition method and blind intervals. `delivered_dynamic_range` in [Models.kt](../../clients/android/app/src/main/java/tv/plurx/app/data/Models.kt) is server delivery evidence, not proof of the physical display. Source DV metadata alone is insufficient. Missing output-grade coverage means `null`; unsupported initial HDR means unavailable. SDR baseline is not applicable, not zero. |
| Delivered vs advertised | Media response bytes ×8 /1000 / elapsed wall seconds over the **final 60 seconds** of each post-cliff stage, compared with the attached rung's advertised `total_kbps`; decimal kb/s. | Save cumulative `shaping.stages[].media_bytes` at the window's two boundaries and subtract, retaining actual observation times/uncertainty. All requests must traverse this sole proxy. This is proxy downstream delivery, not decoder consumption. `measured_media_kbps` uses active transfer span plus first-slice correction, not this 60-second window; admission bytes are different. If boundaries, rung advertisement or attribution are missing, `null`. |

Use the ladder captured from the real decision/session context: `Rung.height`,
`total_kbps` and `peak_kbps` exist in
[Models.kt](../../clients/android/app/src/main/java/tv/plurx/app/data/Models.kt).
Do not substitute source bitrate, video-only bitrate or configured cap for
`total_kbps`. If multiple rungs occupy the final window, split byte/time
accounting at known transitions and report each advertisement; cumulative
proxy counters alone cannot attribute those subwindows, so mark missing when
that attribution is unavailable. The proxy media classification includes HLS
playlists and media paths; disclose that scope and any unrelated media traffic.

**Time alignment:** proxy `transitions[].at_ms` is monotonic elapsed time since
proxy start; `generated_at` and `client_events[].captured_at` are controller
UTC, with the latter marking receipt, not device presentation. Android `ttff.ms`
is an elapsed device duration. Do not subtract these clocks directly. Retain
controller monotonic/UTC anchor readings at start/end, device/capture clock
offset and drift observations, request send/receive brackets, recording time
base and the mapping used. Align a visible/operator marker with its retained
control request and response. If the offset or event delivery delay is unknown,
report timing bounds or `null`; do not invent a send timestamp. Sparse panel
captures cannot fill blind intervals. Final-60-second boundary requests have
latency: retain brackets and actual interval length, label approximations, and
withhold the exact metric if the acquisition cannot establish its window.

## 4. Start one proxy — verified bindings and authentication

These commands are for an approved operator session, not instructions to run
them while authoring the protocol. Node 22+ and an HTTP target are required.
Use a new private run directory and one chosen LAN interface reachable by the
device. Substitute validated host values; no credentials belong in a URL.

```bash
umask 077                                 # keep run files private
run_dir=$(mktemp -d /private/tmp/a04-android.XXXXXX)  # one owned directory
scripts/playback-lab device-proxy \
  --target http://PLURX_SERVER:32400 \
  --listen CONTROLLER_LAN_IP --public-host CONTROLLER_LAN_IP \
  --network-profile 8mbps-to-1.5mbps \
  --control-file "$run_dir/control.json"   # keep foreground for Ctrl-C
```

`--port` is optional; omission binds an OS-selected port. `--listen` is the
actual local bind address; `--public-host` only constructs the advertised
device URL. A wildcard `0.0.0.0` bind requires public-host but exposes all
interfaces; prefer the approved specific address. The target's origin is used,
so a target path prefix is not preserved. The proxy preflights public
`GET /api/v1/server` with a five-second timeout and requires HTTP 200 plus
`instance_id`; that proves reachability, not authenticated playback.

The mode-0600, exclusive-create control file has schema 1 and these real keys:
`schema_version`, `pid`, `proxy`, `target`, `profile`, `status_path`,
`cliff_path`, `evidence_path`, `control_header`, `control_token`. Paths are
`/__playback_lab/status`, `/__playback_lab/cliff`, `/__playback_lab/evidence`;
the header is `x-playback-lab-control`. Keep the random token private, never
paste the control file into a receipt. A missing/wrong token receives 403.

Read the private file locally to call **GET status**, **POST cliff**, or
**POST evidence** with that header. Use a credential-safe local HTTP client
that reads the header value from the file, not shell arguments/logs. Retain
sanitized JSON responses and request send/receive time brackets. POST cliff
advances exactly one stage and returns `applied_at_ms`, `current_stage` and
`current_kbps`; it returns 409 after the last stage. POST evidence returns a
snapshot; it does **not** call `beginEvidence`, reset counters or stop playback.
GET status also returns the snapshot, including cumulative counters and at
most 500 captured client events. Export before truncation; absence of a
truncation flag does not prove a complete event history.

After approval, connect through the app's normal server-entry/login flow using
the exact `proxy` URL. [AppViewModel.kt](../../clients/android/app/src/main/java/tv/plurx/app/ui/AppViewModel.kt)
`connectToOrigin` clears the prior token and saves the new origin, then asks
for login. Do not assume credentials survive an origin change or copy a bearer
token into the shaper control file. The app supplies its ordinary auth;
the proxy forwards request headers/body to upstream and does not mint an app
credential. Control auth is distinct from server login. It neither rewrites
response URLs nor guarantees requests stay routed through it: verify media,
control and beacons use the proxy, including replacements/redirects. Direct
origin media, a second player, downloads or other clients sharing this proxy
invalidate the byte attribution. Do not add an unverified adb intent or deep
link; manually select the recorded title and quality through the existing UI.

## 5. Observe 180 seconds — manual cliffs with retained timestamps

Run only one device/title at a time. Bound startup to 30 seconds waiting for
actual presentation; no first frame means failed startup and no cliff metric.
Record the actual first-frame observation and its uncertainty. Establish the
approved acquisition before playback; keep it through every replacement.

For profile one, request the first cliff approximately 12 seconds after that
frame and retain its exact response timestamp. For profile two, start a fresh
proxy with `8mbps-to-1.1mbps-to-350kbps@12`, make the same first cliff, retain
75 seconds in the 1.1 Mb/s stage, then request the second cliff independently
of recovery. Observe to 180 seconds after the initial frame; each stage then
has a final 60-second accounting window. Record actual stage durations and
timing deviations; shorter stages cannot satisfy that metric. Snapshot bytes
and attached-rung facts at each final-window start/end, before changing stage.
Retain any unresolved recovery through the observation end.

Omit `--auto-advance` for this protocol: the current proxy schedules its first
cliff from captured `ttff`, and later ones only from `ttff` with `reason`
starting `stall-`, using `--recovery-cliff-after` (default eight seconds).
`@12` is the initial automatic delay, not a timer that applies every cliff.
A device that never produces the required recovery beacon would never reach
the second automatic cliff; manual control records that failure without
silently skipping a profile stage. Do not mix the two control modes.

Keep the picture/physical HDR indication and approved capture visible; do not
seek, change quality, pause or background during the measurement. Record any
such action as a deviation with boundaries rather than deleting affected
time. Save Settings → Logs entries for the matching session/attempt after the
run, before retention can erase them. Export proxy snapshots before stopping;
match them to the device and file, not merely to a wall-clock minute.

Stop early on terminal playback, identity change, unapproved concurrent use,
loss of route/capture/HDR evidence, or host resource pressure affecting normal
use. Predeclare a per-run capture allowance (for example 256 MiB) and total
wall deadline (240 seconds including startup); these are operator safeguards,
not limits implemented by `device-proxy`. The proxy's byte ledgers are retained
in memory without a documented cap; it has a 32-socket agent and a 500-event
ring. Observe process/disk headroom and stop the owned run at its allowance;
never extend a failed run until it appears green.

## 6. Stop and restore — preserve evidence before removing control access

Before stopping, POST evidence and retain sanitized responses, all acquisition
artifacts, deviations and identity-after receipts. Ctrl-C the **owned foreground
proxy** (SIGINT); SIGTERM also requests graceful shutdown. It writes mode-0600
`<control-file>.evidence.json` by default, then closes connections and deletes
the control file. `--evidence PATH` can name that output, but is not a device
result-report flag. Confirm exit and listener closure. An abrupt crash/kill may
skip evidence writing or leave the private control file: preserve already
captured snapshots and report the missing final receipt. Never kill by port or
a stale PID; resolve the owned process identity first.

An operator stops only this test playback through normal controls. Under the
explicit restoration approval, restore only the server origin, login/session
and device settings this run changed, using the before record. Origin switching
can require login again; do not promise token restoration or restore by writing
credentials into app storage. Verify normal server connection, no remaining
proxy listener and no unattended test playback. Record incomplete cleanup and
escalate to the operator. Do not restart fleet services, reset device state,
delete other captures or modify a shared feature setting as cleanup.

## 7. Receipt — completeness is separate from behavior

Retain one private raw JSON report per device/profile/pass, with a provenance
manifest and SHA-256 for every referenced artifact. Redact app bearer/query
credentials, shaper token and private media paths from shareable copies; keep
original bytes privately with their original hashes. The following names are
**manual receipt fields**, not claimed Android beacon fields or a new scorer:

```json
{
  "schema_version": 1,
  "suite": "a04-android-manual",
  "summary": {"acceptance": "incomplete", "physical_run_performed": false},
  "results": [{
    "name": "Lenovo Android TV / 8mbps-to-1.5mbps / baseline",
    "status": "incomplete",
    "metrics": {
      "stalled_seconds": null,
      "automatic_switches": null,
      "first_frame_gaps_seconds": null,
      "quality_regained_seconds_per_cliff": null,
      "unexpected_sdr_transitions": null,
      "delivered_vs_advertised_final_60s": null
    },
    "missing": ["No physical run; all acquisition/provenance pending"],
    "raw_artifacts": [],
    "identity_before": null,
    "identity_after": null,
    "time_alignment": null,
    "cleanup": null
  }]
}
```

Each measured field replaces `null` only with its value **and** calculation,
units, uncertainty, interval and raw references in the accompanying receipt.
Use explicit not-applicable for SDR-baseline HDR transitions, with the source
grade reason; it does not fill the Dolby Vision pass. Failed startup, absent
second cliff, unknown grade, missing intent, censored frame gap and missing
byte window stay visible. A complete observation can still fail behavioral
acceptance; an incomplete report can never pass D3.

```bash
scripts/playback-lab normalize --json RAW_REPORT.json --out NORMAL_REPORT.json
                                             # comparison companion only
```

The current normalizer accepts schema 1 plus `results`, then removes timings
and most detailed metric/provenance fields. It is not a six-metric scorer or
proof that a manual report is complete. Keep raw report, normal companion and
hashes together; never replace the raw six-metric receipt with the normalized
projection. Append actual results under a dated heading in the design only
after a physical run, naming the exact build and missing fields. D3 remains
open until the design's whole platform/profile matrix is supported; this
protocol closes only the missing written Android procedure deliverable.
