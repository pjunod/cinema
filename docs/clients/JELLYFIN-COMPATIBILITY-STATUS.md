# Jellyfin compatibility — measured build progress and remaining gates

**Status:** open · J0 in progress; J1–J6 not started · **Updated:** 2026-10-02 EDT.

Companion to [the reviewed build contract](JELLYFIN-COMPATIBILITY-BUILD.md)
(what must be built and proved) — this records execution and evidence. The
Emby phases in [the outline](JELLYFIN-EMBY-COMPATIBILITY-OUTLINE.md) remain
outside this build. A reference-server response is not physical playback
acceptance.

## 1. Contract publication — the real gate ran before merge

[PR #744](http://192.168.4.7:3000/noirr/plurx/pulls/744) published the reviewed
contract. Its earlier runs were skipped because their event payloads carried
`draft: true`; making the PR ready had not produced a new eligible run.
Closing and reopening the ready PR triggered the existing fast lane without
changing the reviewed source or workflow.

[Run 3881](http://192.168.4.7:3000/noirr/plurx/actions/runs/3881) passed scope,
policy/contract preflight and **Main promotion gate** on head `b3ff464b9` and
base `9a719fcb7`. Unaffected compiler jobs were skipped by the docs-only
scope. The gate checked that both refs remained current. PR #744 then merged
as `772b9cf23`; `effort/jellyfin-compat` starts there.

The [publication record](jellyfin/docs-publication-receipt.json)
retains those identities and job outcomes. It is ordinary docs fast-lane
evidence, not the final effort qualification receipt. The shared primary
checkout and its unrelated work were preserved.

## 2. J0 baseline — pinned reference, pending physical flows

The [baseline manifest](jellyfin/baseline-manifest.json)
records immutable provenance and hashes for synthetic fixtures. Exact versions
are established separately from interoperability:

| Surface | Measured or pinned baseline | Evidence boundary |
|---|---|---|
| Reference server | Jellyfin 10.11.11, upstream commit `1fbd8739292cce610231be93daf43368733edf63`; official container index digest and arm64 image ID in manifest | Disposable synthetic library; not a Plurx deployment |
| Schema | OpenAPI served by that container under `/jellyfin`, 315 paths; SHA-256 in manifest | Raw hash includes the served origin; separate canonical hash removes only `servers` |
| Infuse | Installed 8.5.6 / 8.5.5763 on connected physical AppleTV14,1, tvOS 27.0 / 24J361 | Physical reference flows measured; native fMP4 renders with exact init-prefix wrapper; production adapter pending |
| Android TV | Installed official release 0.19.10 / 191099, source commit `984181a3d6ab14e9a6d2dcc850c582e1c138bd95`; released APK SHA-256 in manifest | Physical Google TV Streamer connected; Android 14 / API 34 / UTTK.260317.003 measured; Media3 ExoPlayer 1.8.0 measured from current app runtime logs |
| Android TV dependencies | Kotlin SDK 1.7.1, Media3 1.8.0 from pinned `gradle/libs.versions.toml` | Source provenance, not device capability evidence |

Server provenance is [the official released tag](https://github.com/jellyfin/jellyfin/releases/tag/v10.11.11).
The Android candidate is [the official release](https://github.com/jellyfin/jellyfin-androidtv/releases/tag/v0.19.10).
The pinned Android profile branches at server 10.11 for Dolby Vision range
predicates and declares VTT external/HLS delivery. Both clients’ negotiation and ancillary calls are captured below.

**Synthetic corpus:** an eleven-minute 1280×720, 24 fps H.264/AAC MP4; a
matching MKV with English/French AAC audio and an English SRT track; external
SRT cues at 1, 120 and 330 seconds; one local poster. The manifest records file
hashes and sizes. This corpus supports direct, remux, forced bitrate encode,
track selection and a pause longer than 300 seconds; it does not satisfy the
HDR/DV acceptance row. Two episode aliases of the same synthetic MP4 now form the TV corpus without
duplicating source bytes; both clients browsed the series and season, and Infuse played the first episode.
Android also requested offset 1 and received the second episode. A full multi-page
corpus and the HDR corpus remain to be prepared. The MKV was regenerated after
a disk-space cleanup; the current manifest records its new container hash.

**Reference preparation:** dedicated random-password test account, synthetic
movie library, `/jellyfin` base path, automatic port mapping disabled. A
temporary proxy binds to the controller's LAN address and removes credentials,
pseudonymizes IDs and omits non-JSON bodies before writing traces outside the
repository. Its harness login trace was checked for the original password,
token and user UUID; none appeared. Raw protocol credentials and personal
device identifiers are not retained in this repository. Prototype files and
downloads remain outside maintained source while J0 is active; remove owned
scratch after retaining minimized fixtures and findings.

| Probe | Result | Counts as |
|---|---|---|
| Harness public system info, public users, Quick Connect status under `/jellyfin` | 200, JSON read successfully | Reference bootstrap only |
| Harness authentication | 200; dedicated account authenticated | Reference account only |
| Harness movie projection | Eleven-minute movie, one media source | Reference library preparation only |
| Infuse connect/browse/direct/seek/transcode | Physical Infuse 8.5.6 connected; movie details/artwork, static MKV and explicit 750 Kbit/s encoded HLS rendered; forward/backward seeks and 374.353-second same-play pause/resume measured | Native transport wrapper proved on one fixture; production qualification pending |
| Android TV connect/browse/HLS/remux | Connect, password login, movie details/artwork, direct MKV and 720 Kbit/s encoded HLS rendered on physical TV; 332.6-second pause/resume, forward/backward seek and app-kill replacement measured | Required J0 client gate |
| Both clients' image/subtitle/media credential carriers and version-implied calls | Android: anonymous artwork/direct media; ApiKey on SRT and HLS; MediaSegments and Intros observed. Infuse: authorization plus API-key HLS query; authorization and Range for SRT/VTT | Required policy/design evidence before J2 |
| Client pause over 300 seconds, kill/background, renegotiation without old Stopped | Android reference HLS: 332.6-second same-play resume, abrupt kill then new play without old Stopped. Infuse: 374.353-second same-play resume and SIGKILL replacement without old Stopped. Native VOD reap confirmed; recovery returns 410 | Required lifecycle evidence before advancing J0 |
| Native encoded VOD without copy index; bounded capacity | Service regressions passed, including decoded GET bytes after forward/back restarts | Native seam evidence only; client activation and per-create policy pending |
| Native unindexed HEVC copy and preparation deduplication | Service regression passed; refused copy and one preparation request, no incomplete VOD session | Native seam evidence only; facade immediate fallback pending |
| Missing duration | New copy/encoded regression refuses absent, zero and negative durations without attaching sessions/renditions | Native prerequisite evidence; HTTP/client mapping pending |
| Native prototype create with positive duration, missing index | Copy: immediate `vod_index_pending`; encoded: 200 in about 0.13 s, closed fMP4 playlist, physical Android renders native fragments | Single-fixture alias spike; production adapter and per-create policy pending |
| First-minute engine attestation | Ready at 2.931 s; immediate encoded request returned 200 in 1.732 s; media-engine spawn attestation 0.048434 s | Isolated restart measured within first minute; transient/persistent fault outcomes below |
| Long-pause real-client resurrection and VOD-only worker/relay propagation | Native idle reap measured; recovery fails with 410; worker envelope unproved | Required J0 hard-seam experiments |

### Physical Android TV — reference recovered; direct and encoded HLS rendered

Paul made an idle Android TV available. It is a **Google TV Streamer**, not
the separate TCL 9445X from the earlier fleet inventory. Its IP responded,
and a separate J0 ADB server connected to the existing network-debugging
port without restarting the shared ADB server. Android TV/Leanback hardware
features, OS/build and installed app version were measured.

Jellyfin was absent, so the pinned official release APK was installed after
its SHA-256 matched the manifest. The app launched and accepted input of the
reference URL. Its actual request retained `/jellyfin/System/Info/Public`
and supplied MediaBrowser client/device metadata. This is one observed
bootstrap request, not proof of all base-path or credential behavior.

The [sanitized connection and playback observation](jellyfin/androidtv-connection-observation.json)
retains the initial upstream `ECONNREFUSED` and the later successful flow.
Docker's guest VM had stopped and Desktop remained stuck in `stopping`;
a normal restart timed out. Automatic approval review initially rejected
terminating the stuck shared Docker processes. Paul then explicitly approved
force-close recovery. Verified Docker Desktop processes were terminated,
Desktop restarted, and the existing reference container and data recovered.
The engine reported 29.7.2; server bootstrap returned Jellyfin 10.11.11.

The client connected, offered Quick Connect and password authentication,
authenticated the dedicated account by password, and displayed the synthetic
movie details and artwork. The MKV rendered visible test-pattern frames and
reported `DirectPlay`. Selecting 720 Kbit/s sent `Stopped` for the direct play,
then negotiated a new play at the current source position and rendered
encoded HLS. The negotiated profile declares MPEG-TS (`ts`) HLS; actual
requests use `master.m3u8`, `main.m3u8` and `/hls1/main/<segment>.ts`.
Current-app `ExoPlayerImpl` initialization logs identify AndroidX Media3 1.8.0.
This proves reference-server playback only, not Plurx adapter acceptance.

The default trace requests `Stream.subrip` and selects TS HLS. Controlled
probes below establish that this pinned client can render VTT and normalized
fMP4 when negotiation selects them. The default TS flow alone is not evidence
for native fMP4 aliases. Image GETs and static direct-media GETs carried neither header nor
query credentials in the observed run; subtitle and HLS requests carried
`ApiKey` in the query. API calls carried MediaBrowser `Authorization`.
The client requests `MediaSegments` and `Intros`; semantic empty results
remain part of the candidate subset. These are observations for this pinned
client, not universal Jellyfin policy. Response JSON was initially gzip and
omitted by the capture parser; the corrected bounded decompressor now retains
sanitized negotiation responses. Repeated-ID relationships are scoped to
each proxy capture session.

The encoded HLS play remained paused for **332.632 seconds** between the
first paused and first resumed progress reports. Twenty-three paused reports
retained one position; resume kept the PlaySessionId and rendered moving
frames. Jellyfin was not forced to reap this producer. Continued paused
heartbeats mean this is external-client timing evidence, not proof of the
native VOD reader-resurrection path. Forward seek advanced from approximately
80 to 200 source seconds and rendered a 214-second test-pattern timestamp.

Abruptly force-stopping and relaunching the client emitted no `Stopped` for
the old play. The client resumed from 230.198 source seconds with a new
PlaySessionId and rendered a 234.708-second frame. DeviceId remained stable
within this capture session. Directional backward seek then rendered an
approximately 185.567-second frame, with a corresponding decrease in progress
position. These observations support the planned stable-player/new-delivery
identity and source-position translation rules. Native replacement/retirement,
terminal stop, forced idle reap and background behavior still need adapter
experiments. Playback was stopped after the probe.

### Controlled VTT/fMP4 — native normalization matters

A temporary proxy changed the upstream DeviceProfile, separately recording
the original client body and the override. VTT-only negotiation over the
reference TS stream rendered an English cue at the two-second test timestamp.
The wider fMP4 override first produced an honest server 400: the original
client's long audio-codec list exceeded the reference query validator's bound.
Narrowing the probe to H.264/AAC produced valid HTTP 200 media, but Media3
rejected the copied AAC fragment's negative `tfdt` with
`readUnsignedLongToLong` / “Top bit not zero: -1008”.

A labeled variant in the dedicated reference container changed only FFmpeg's
`-avoid_negative_ts disabled` argument to `make_zero`, matching native
`vodgen.rs` normalization. It rendered fMP4, forward/backward seeks, VTT and
French audio; a pause lasting more than 325 seconds resumed the same
PlaySessionId with advancing frames. The minimized observation retains the
wrapper source/hash and exact probe boundaries. The original reference
encoder, preset and transparent proxy behavior were restored afterward.
This is a transport spike, not acceptance of unmodified Jellyfin fMP4 or of
Plurx's unbuilt negotiation/profile translator.

A second temporary alias forwarded one dedicated native session's exact
playlist, init and fragment names. Every alias requires a fresh 30-minute
capability; the proxy retains neither it nor the native credential. Only the
negotiated PlaybackInfo response is overridden. Android appends the configured
`/jellyfin` base prefix, so the returned media URL must omit that prefix rather
than double it. Native H.264/AAC fMP4 rendered visible test-pattern frames.
This deliberately uses reference identities/events and one native fixture;
it proves delivery compatibility, not track mapping, auth or a full facade.
The measured pause lasted **343.445 seconds**, with 22 paused progress reports.
Operator activity showed the native reader gone, and the native log confirmed
idle reaping. Resume plus a seek beyond the buffer requested native init and
playlist bytes, which returned **410 `media_session_ended`**.

This exposes a durable lifecycle gap: the owner lease loop classifies the
idle-reaped VOD reader as a stale worker and terminalizes its route. Public
resurrection requires an active durable route, so the existing isolated
reader-transition regression does not cover the failing owner integration.
No native control or progress-to-producer touch was synthesized in this
probe. J0 must settle a bounded passive route-retention policy and update the
contract before J4; raising the producer TTL would conceal this failure.

### Engine attestation — restart and bounded fault experiment

A warm-storage restart became ready at 2.931 seconds. The first encoded
request was sent immediately and returned 200 in 1.732 seconds, within the
first minute; its playlist was closed VOD. The engine histogram measured one
media spawn attestation at 0.048434 seconds and two stat charges totaling
0.000462 seconds.

The fault experiment copied the configured encoder into owned scratch, then
made only its directory temporarily unavailable after successful attestation.
Four native creates refused with `vod_engine_unattested`; restoring the same
executable allowed the same request identity to succeed at 4.062 seconds.
Holding the path unavailable produced seven honest refusals and stopped at
12.128 seconds under one 15-second deadline. The synchronous harness used a
finite backoff schedule; it created no detached retry task. This proves the
native refusal and recovery seam, not an already-built facade retry policy.
The scratch encoder was restored and the isolated daemon stopped afterward.

## 3. Compiler loop — available before Rust changes

The local toolchain resolves through `/Users/pjunod/.cargo/bin` with
`RUSTUP_TOOLCHAIN=1.97.1`; `rustc --version` reported
`rustc 1.97.1 (8bab26f4f 2026-07-14)`. All-target `cargo check -p plurxd
--all-targets --locked` passed on the reviewed publication source before any
Rust edit. This uses the local compiler, not CI as a compiler.

The existing `vodserve::tests::idle_reap_and_same_id_resurrection_are_one_reader_transition`
regression passed: one test executed, 3,137 filtered. It proves the native
reader-transition race is fenced. It does not prove either external client's
pause/resume behavior or the new facade's adapter, which has not been built.

Additional native service regressions ran on the integrated base, each with
one test executed and 3,137 filtered:

| Test (`cargo test -p plurxd --bin plurxd <name> -- --nocapture`) | What actually passed |
|---|---|
| `encoded_vod_ntsc_gets_decode_after_forward_and_backward_restarts` | Encoded VOD attaches without a copy index; fetched init/segment bytes decode after 0, 90.09 and 3-second starts, with stable initialization bytes |
| `encoded_vod_capacity_read_has_one_deadline_and_no_orphaned_wait` | Existing capacity policy has a bounded read deadline and no orphaned wait |
| `first_play_queues_missing_source_preparation_with_discovery_off` | Missing HEVC copy preparation refuses; repeated first play queues one existing preparation request and attaches no incomplete VOD session |

A new regression,
`copy_and_encoded_vod_refuse_missing_or_nonpositive_duration_without_attachment`,
passed with one test executed and 3,138 filtered. It checks absent, zero and
negative duration on valid copy and encoded recipes, typed
`vod_source_unsupported`, and no attached sessions or renditions.

The preceding tests are native service evidence, not a new compatibility adapter or a proof
that an external client will request the resurrection path. The per-create
VOD-only policy is implemented and tested below; actual worker execution remains unproved. The passive retention
spike now has the native evidence recorded below; physical recovery is pending. Test linking
reported the macOS compact-unwind size warning; execution passed. Workspace
Clippy with `-D warnings`, formatting, catalog lint and served JavaScript
syntax passed through the normal tracked hook.

### Infuse physical reference flow

[The minimized Infuse observation](jellyfin/infuse-connection-observation.json)
records Apple TV 4K (third generation), tvOS 27.0 / 24J361 and Infuse 8.5.6
build 8.5.5763. Password login, movie detail and artwork requests retain the
configured `/jellyfin` base path. Static MKV renders with `Range: bytes=0-`
and an authorization header; Infuse reports `DirectStream` even though this
is static file delivery, so that label alone does not prove server remux.

The details context menu's Transcoding → 0.75 Mbps (480p) action sends a
750,000-bit/s play ceiling. Its profile omits `DirectPlayProfiles`, advertises
TS HLS with `hevc,h264,av1` and AAC, and advertises external VTT/ASS/SSA.
A subsequent track request names `CurrentPlaySessionId` and replaces the
negotiation before `Playing`. Encoded TS HLS renders. Media requests carry
an authorization header and an API-key query. After a 374.353-second pause,
the same play resumes and a 120-second seek renders at 225.417 seconds.
This proves the reference client behavior, not native route resurrection.

The initial sync also asks for `UserItems/Resume`, `UserViews/GroupingOptions`,
`Library/VirtualFolders`, `DisplayPreferences/usersettings`, `Items/Latest`
and `RandomSeriesItems`. Freeze the subset only after episode and subtitle
flows. Abrupt SIGKILL and relaunch resume at 285.333 seconds with a new play id, the
same device identity and no old `Stopped`. An external SRT cue renders at
124.583 seconds. Although the reference advertised `Stream.vtt`, the original
`Codec=subrip` metadata made Infuse construct `Stream.srt`; a controlled
`Codec=webvtt` response instead produces `Stream.vtt` with HTTP 200/text-vtt.
The VTT opening cue renders at 2.125 seconds. The probe changes only
subtitle delivery metadata; the reference responses are restored. Native
fMP4 renders through the measured initialization-prefix wrapper described below. Series/season/two-episode browsing and first
episode playback render; initial paging requests use series limit 50 and
season limit 200. A subsequent-page corpus remains unproved.

### Infuse native transport — initialization is required per fragment

The controlled native delivery experiments are retained in the same Infuse
observation. Supplying a media playlist directly fails before any fragment
request. A master wrapper makes Infuse fetch its child under
`/jellyfin/Videos/{item}`; even an absolute child is prepended there. Item-relative
routes reach the native fragment, but Infuse requests no `EXT-X-MAP` object and
fails. Changing `TranscodingContainer` from `mp4` to `fmp4` does not alter that.

Prefixing the exact 1,275-byte native initialization object to each native
fragment and declaring the exact combined content length renders without a
second encoder or timestamp rewrite. Physical source times are 2.333 seconds
at opening, 144.958 after a forward 120-second seek and 114.758 after a backward
60-second seek. After a measured 353.303-second pause, the reader was idle-reaped;
resume and a seek outside the prefetched window returned 410 for four native
fragment requests. Infuse displayed “Server responded with status 410”. One
paused report at/after the final pause boundary was captured; reports reached
only the reference server and did not touch the native reader. Omitting the combined length instead produces Infuse’s explicit
“Server didn’t report the size of the file” error. This is a single-fixture
transport spike; production native publication, range, cancellation and terminal
fences still need tests. The client-specific capability rule and wrapper must
be proved in J4 rather than treating Infuse’s declared TS profile as fMP4 proof.

The internal per-create `vod_only` policy now refuses unindexed copy before
rolling allocation with recovery globally enabled. Three focused regressions
prove that refusal, worker/durable JSON preservation and distinct identity,
and native HTTP ignoring the client-supplied policy knob. Actual worker
dispatch remains unproved. The passive route spike now passes ten focused
regressions, including real reader detach/resurrection, current-play identity,
quotas, terminal/replacement fencing, reader-free owner renewal and expiry,
and a public fragment GET against real indexed media retaining the same
durable incarnation and owner epoch. Native HTTP defaults and the existing
idle-reap/resurrection race also pass. Physical long-pause recovery and the
remaining facade/worker/race boundaries are still pending; see the
[passive lifetime ADR](JELLYFIN-PASSIVE-VOD-ROUTE-LIFETIME.md#native-spike-evidence--actual-owner-renewal-and-fragment-recovery).

## 4. Milestone admission — J0 remains open

| Milestone | State | Remaining admission evidence |
|---|---|---|
| J0 | In progress | Both required physical flows; carrier/base-path traces; lifecycle/preparation experiments; frozen protocol subset and revised estimate |
| J1 | Waiting on J0 | Identity, deterministic retirement/replacement guard, token-only service seams |
| J2 | Waiting on J1 and traced artwork policy | Connect/browse, bounded misses and cold-node first sync |
| J3 | Waiting on J2 | Direct/watch, Store revisions and per-play final durability |
| J4 | Waiting on J3 and J0 hard-seam decisions | VOD-only negotiation/activation, refusal mapping, bounded retries and aliases |
| J5 | Waiting on J4 | Track, subtitle and observed ancillary completion |
| J6 | Waiting on J5 | Frozen physical/cluster matrix, qualification and graduation |

The Android TV reference flow now renders direct and encoded HLS playback.
Native idle recovery and production Infuse transport qualification remain open.
Both physical clients have connected and played reference direct/encoded media;
both have rendered native fragments under controlled transport probes.
Unperformed operations remain **not tested**. Do not mark
J0 complete, start the full facade route build or reduce the required client
matrix to compensate. No compatibility release, setting graduation or fleet
deployment has occurred.
