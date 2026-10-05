# Jellyfin compatibility — measured build progress and remaining gates

**Status:** open · J0–J3 integrated; J4 service work complete pending integration; physical client matrix deferred · **Updated:** 2026-10-04 EDT.

Companion to [the reviewed build contract](JELLYFIN-COMPATIBILITY-BUILD.md)
(what must be built and proved) — this records execution and evidence. The
Emby phases in [the outline](JELLYFIN-EMBY-COMPATIBILITY-OUTLINE.md) remain
outside this build. A reference-server response is not physical playback
acceptance.

## 1. Contract publication — the real gate ran before merge

[PR #744](http://forge.lan:3000/noirr/plurx/pulls/744) published the reviewed
contract. Its earlier runs were skipped because their event payloads carried
`draft: true`; making the PR ready had not produced a new eligible run.
Closing and reopening the ready PR triggered the existing fast lane without
changing the reviewed source or workflow.

[Run 3881](http://forge.lan:3000/noirr/plurx/actions/runs/3881) passed scope,
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
| Client pause over 300 seconds, kill/background, renegotiation without old Stopped | Android reference HLS: 332.6-second same-play resume, abrupt kill then new play without old Stopped. Infuse: 374.353-second same-play resume and SIGKILL replacement without old Stopped. Initial native recovery returned 410; corrected Android recovery passes after 373.404 seconds; Infuse repeat pending | Required lifecycle evidence before advancing J0 |
| Native encoded VOD without copy index; bounded capacity | Service regressions passed, including decoded GET bytes after forward/back restarts | Native seam evidence only; client activation and per-create policy pending |
| Native unindexed HEVC copy and preparation deduplication | Service regression passed; refused copy and one preparation request, no incomplete VOD session | Native seam evidence only; facade immediate fallback pending |
| Missing duration | New copy/encoded regression refuses absent, zero and negative durations without attaching sessions/renditions | Native prerequisite evidence; HTTP/client mapping pending |
| Native prototype create with positive duration, missing index | Copy: immediate `vod_index_pending`; encoded: 200 in about 0.13 s, closed fMP4 playlist, physical Android renders native fragments | Single-fixture alias spike; production adapter and per-create policy pending |
| First-minute engine attestation | Ready at 2.931 s; immediate encoded request returned 200 in 1.732 s; media-engine spawn attestation 0.048434 s | Isolated restart measured within first minute; transient/persistent fault outcomes below |
| Long-pause real-client resurrection and VOD-only worker/relay propagation | Corrected Android recovery passes; trusted worker startup and activated relay pass; Infuse repeat pending | Required J0 hard-seam experiments |

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

The local toolchain resolves through `~/.cargo/bin` with
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
and native HTTP ignoring the client-supplied policy knob. Actual exact-auth
HTTP worker starts now ran from an isolated voter to a distinct live learner
with both internal policies selected and rolling recovery globally enabled:
unindexed copy returned 422 in 4.4 ms; encoded VOD returned 201 in 1.403 s
with an activation generation. Its unconfirmed provisional worker later
received native terminal cleanup. A subsequent ordinary 480p placement selected that learner, retained both
policies in its active durable recipe, and delivered playlist/init/fragment
200 responses through the voter relay. Stop returned 204 and a late relayed
fragment returned typed 410. Facade activation, physical remote-worker
decoding and transparent owner failover remain separate acceptance items. The passive route spike now passes ten focused
regressions, including real reader detach/resurrection, current-play identity,
quotas, terminal/replacement fencing, reader-free owner renewal and expiry,
and a public fragment GET against real indexed media retaining the same
durable incarnation and owner epoch. Native HTTP defaults and the existing
idle-reap/resurrection race also pass. Physical Android recovery now passes after a 373.404-second pause: zero
readers before resume, active same-incarnation route/owner, new fragment 200s,
visible source time 302.917 seconds, and typed late 410 after native Stop.
The controlled build selects the service policy only in an archived probe
snapshot; production HTTP defaults remain unchanged. Infuse switched away
from the synthetic test during the wait, so its recovery repeat and the
remaining facade/worker/race boundaries are still pending; see the
[passive lifetime ADR](JELLYFIN-PASSIVE-VOD-ROUTE-LIFETIME.md#native-spike-evidence--actual-owner-renewal-and-fragment-recovery).

## 4. Milestone admission — physical Infuse repeat moves to J6

| Milestone | State | Remaining admission evidence |
|---|---|---|
| J0 | Admitted for implementation by user direction | Corrected physical Infuse repeat retained as J6 qualification requirement |
| J1 | Ready after J0 task integration | Identity, deterministic retirement/replacement guard, token-only service seams |
| J2 | Integrated into effort; anonymous artwork decision pending | Connect/browse, bounded misses and cold-node first sync |
| J3 | Integrated as `13568fbf3`; all eight effort gates passed | Direct/watch, Store revisions and per-play final durability |
| J4 | Profile predicates and probe fencing implemented; native VOD integration underway | VOD-only negotiation/activation, refusal mapping, bounded retries and aliases |
| J5 | Waiting on J4 | Track, subtitle and observed ancillary completion |
| J6 | Waiting on J5 | Frozen physical/cluster matrix, qualification and graduation |

The Android TV reference flow now renders direct and encoded HLS playback.
Corrected native idle recovery passes on Android; production Infuse transport
qualification remains open.
Both physical clients have connected and played reference direct/encoded media;
both have rendered native fragments under controlled transport probes.
Unperformed operations remain **not tested**. Do not mark
physical Infuse qualification complete or reduce the required client matrix.
On 2026-10-02 Paul explicitly deferred the remaining Apple TV test until the
end and authorized proceeding with implementation. J1–J5 may now proceed;
the corrected Infuse repeat remains required in J6 on the frozen candidate. No compatibility release, setting graduation or fleet
deployment has occurred.

## 5. Measured protocol candidate and revised estimate

The pinned observations now cover movie playback, series/season/episode
navigation, subtitle URL construction and replacement without an old Stopped
report. They support the following candidate subset for J1–J5. The user-directed deferral permits freezing this implementation subset now,
with corrected Infuse recovery reserved for J6. A later client trace which
needs an additional route requires an explicit contract amendment.

| Request family | Candidate behavior justified by the retained traces |
|---|---|
| Bootstrap and authentication | Public system/user information, password authentication and observed Quick Connect capability behavior under the configured base path |
| Catalog and navigation | User views, grouping options, virtual folders, item detail/list/latest/resume, NextUp, series seasons/episodes and observed RandomSeriesItems |
| Artwork | Observed primary/backdrop paths, cache tags and bounded native artwork service; Android's anonymous image requests require the reviewed artwork policy |
| Preferences and ancillary | Observed display preferences, local trailers, special features, MediaSegments and Intros with their defined semantic empty results |
| Negotiation and media | PlaybackInfo, authenticated direct Range delivery, native immutable VOD aliases, and the measured Infuse master/item-relative playlist plus init-prefix representation |
| Tracks | Requested audio selection and VTT metadata/delivery; native extraction and selection remain subject to J5 validation |
| Playback reports | Playing, Progress and Stopped scoped to the authenticated device/current play; missing old Stopped cannot keep a predecessor alive |

A malformed `/Items//` 404 in the trace is not a route to implement. Reference
TS requests establish negotiation behavior; the measured native fMP4 probes
define the candidate transport. First-page browsing does not qualify full
library paging, and observed WebSocket handshakes do not justify inventing a
remote-control implementation.

**Revised planning estimate:** 6–10 engineer-weeks cumulative for the useful
release, versus the reviewed 5–9 week estimate. This is an engineering estimate,
not a calendar completion promise. The additional allowance covers passive
route lifetime integration, Infuse's measured representation and their
publication/race/physical qualification. The native policy and Android proof
are already implemented, but facade identity/watch storage and the complete
physical/cluster matrix remain substantial work. Re-estimate after J1's
storage seams and J4's production transport tests; passing the J0 spike does
not make those milestones complete.

## 6. Native ownership inventory — J0 additions reviewed

The first manual effort run failed the mechanical ownership census. Its ten
count changes were reviewed against the contract base: existing resurrection,
supersession, publication and release calls added only by the new regressions;
two aborted-and-joined fixture tasks; three bounded fixture waits; one
monotonic `Instant` import for grant expiry; and one Axum response-status
assertion which launches no process. The
[ownership inventory](../../tests/playback/rolling-producer-owners.toml) now
records each exact delta and its owner. The passive grant creates no producer,
process or independent timer; existing maintenance and durable owner renewal
remain its lifecycle owners. The failed run is not integration evidence;
rerun the local preflight and the manual effort gate on the corrected tree.

## 7. Integration gate — Windows compiler resource recovery

The corrected ownership tree at `e9dfdc6fc` passed effort policy/contracts,
Rust, web, Apple and Android compilation. The Windows lane reached its
cross-build but its daemon compiler was killed with `SIGKILL`; that failed
candidate is not merge evidence. An earlier attempt failed while resolving the
pinned toolchain action, before source compilation.

The shared Windows compile action now sets one Cargo build job and omits
Dev/Test debug symbols to reduce simultaneous compiler/linker memory pressure.
It still builds the entire workspace and all targets with the lockfile,
repository-pinned compiler, pinned Windows SDK/CRT and embedded manifest.
The existing workflow contract checks these resource bounds alongside target
coverage. No shared runner cache was cleared and no toolchain pin was changed.
Rerun the effort gate on the updated candidate before merging J0.

## 8. Integration gate — wait for received browser evidence

Effort run 3958 on `3a0310a33` passes policy/contracts and every compile lane,
including Windows. Its browser fixture failed the local-seek assertion even
though the video reached the requested position and `session_creates` stayed
zero. The fixture sampled asynchronous `clientLog` POSTs after a fixed 250 ms
sleep, which cannot establish that the server has received a fire-and-forget
message.

A controlled localhost reproduction adds one second before recording only the
`seek_local` POST. The unchanged fixture fails with the same missing-log
assertion while the seek succeeds. The corrected fixture waits on a condition
for that exact event within its existing 38-second deadline before snapshotting
the evidence. It retains both the log assertion and zero-new-session assertion.
The delayed HTTP regression passes, and the full browser fixture passes with
the same injected delay in worker and inline-fallback modes on Headless Chrome
154 with vendored hls.js 1.6.16. No application seek behavior or playback
assertion changed. The new candidate still requires its complete effort gate.


## 9. J0 integrated; J1 foundations under verification

[Effort run 3947](http://forge.lan:3000/noirr/plurx/actions/runs/3947)
passed every required lane on `adf0d5e89`: policy/contracts, Rust, Windows,
web, Apple, Android and the aggregate Effort development gate.
[PR #747](http://forge.lan:3000/noirr/plurx/pulls/747) merged into the effort
as `0331f5686`, retaining all thirteen checked regression fields in the landing
message. This closes the J0 integration failures described above. It does not
qualify the remaining physical Infuse recovery or the production facade.

[PR #763](http://forge.lan:3000/noirr/plurx/pulls/763) consolidates the J1
[protocol forms](JELLYFIN-PROTOCOL-FOUNDATION.md),
[durable identities](JELLYFIN-DURABLE-IDENTITIES.md),
[shared services](JELLYFIN-SHARED-SERVICES.md) and
[play bindings](JELLYFIN-PLAY-BINDINGS.md). The foundations include scoped
password-fenced login replacement, exact native capability references,
source-incarnation checks, terminal tombstones and atomic pending admission
limits. J1 merged the current J0 effort base in `b154a5055`; its focused
regressions and mandatory checks are being repeated against that combined tree
before publishing the candidate for the blocking effort gate.

Public routes, native resource-release adapters and manual-watch revision
fences remain subsequent milestone work. No public compatibility playback is
exposed by J1. The Apple TV remains untouched under Paul's explicit deferral;
its availability does not block implementation or the J1 integration gate.


## 10. J0–J3 integrated; J4 negotiation underway

[Effort run 3963](http://forge.lan:3000/noirr/plurx/actions/runs/3963)
passes all eight required jobs on J1 head `0e4318355`.
[PR #763](http://forge.lan:3000/noirr/plurx/pulls/763) merged as `d3719303e`
with exactly `0331f5686` and `0e4318355` as parents and all 36 checked
regression references in the landing message. The earlier run's Windows
failure occurred while fetching the pinned toolchain action, before source
compilation; the unchanged candidate passed on retry. Protocol-only draft
PR #757 closed after its complete implementation and regressions were folded
into #763.

J2 starts from this integrated effort. The connection facade now uses native
password authentication, token expiry/revocation and an atomic switch generation
that fences late login replacement across disable/re-enable. Its catalog reads
use one coherent Store snapshot for paging, hierarchy, source facts and each
user's watch state. Global probe indices retain zero; native paths never appear
in the DTOs. Upcoming and similar rows use native dates and metadata.

All sixteen Jellyfin Store contracts pass against SQLite and the real
three-voter backend, including 2,500 tied-sort items, empty-page totals,
user/source retirement, catalog rails, artwork mappings and switch generations.
The 28 ownership/SQL censuses and transaction classification also pass. HTTP
regressions cover connection, strict credential conflicts, zero paging, disabled
JSON routes and preservation of the native shell. Artwork miss regressions
prove bounded admission, demand deduplication and no inline original hashing or
decoding. The native owner also publishes the cold derivative successfully;
warm responses use private revalidation and conditional hits add no hashing
or decoding. Old queued work is discarded across switch generations.

Artwork routes currently require authentication. Automatic approval review
rejected anonymous mapped artwork exposure; the direct human decision is
pending. Neither anonymous behavior nor physical client browsing is qualified.
The Android TV's previously authorized address is currently unreachable. The
Apple TV remains deferred and untouched under Paul's instruction; these device
checks do not block implementation or automated compilation.

[Effort run 3983](http://forge.lan:3000/noirr/plurx/actions/runs/3983)
passed all eight jobs on J2 head `5a5514b7f`. [PR #772](http://forge.lan:3000/noirr/plurx/pulls/772)
merged as `e5d17b1c8` with the eighteen checked regression references in its
landing message. The full local web check passed without increasing the
TypeScript ratchet. Artwork remains authenticated pending the direct decision.

J3 runs on that integrated tree. Manual revision, guarded queue admission and
commit, atomic final/terminal writes, and exact-login logout have focused
SQLite and real three-voter evidence. Import drops saved edit context while
preserving counters and existing fences. Released bindings cannot gain an
own-edit exemption while waiting for a failed final to retry.

All fifteen HTTP/shared-progress regressions pass, including direct Range and
HEAD, positionless Stop, cross-login rejection, profile separation, injected
final-write failure/retry, and logout preserving another device's token and
native file grant. The native watch contract also passes on both backends;
27 native watch unit regressions, 28 SQL/read censuses and two transaction
inventories pass. Workspace/all-target Clippy passes on pinned Rust 1.97.1. All 22 Jellyfin
Store contracts pass on SQLite and three voters (214.11 seconds), and all ten
shared coalescer regressions pass. Route deadline and metric-group inventories
also pass. These results do not qualify either physical client,
J4/J5 transport completion, or release promotion.


[PR #779](http://forge.lan:3000/noirr/plurx/pulls/779) merged J3 as
`13568fbf36b415f111d922c33faf90fdcbd54c83`, with parents `e5d17b1c8` and
`a7d45e149` and all 22 checked regression references in the landing message.
[Effort run 4018](http://forge.lan:3000/noirr/plurx/actions/runs/4018)
passed all eight jobs. The exact J1 history erratum records its earlier landing
whose subject omitted the PR number; it does not weaken the history audit.

The first J4 slice evaluates bounded codec/container predicates against a
coherent native file/probe snapshot and the selected audio track. Unknown
predicate names/operators and malformed values refuse. Known missing facts
retain the pinned reference's `IsRequired` semantics; they do not invent HDR
or Dolby Vision capability. Negotiation stores the exact nullable probe, and
both current-play admission and durable progress reject a changed probe even
when size and modification time match. A legacy J3 binding without that new
field keeps its existing source fence. Each ordered transcoding profile keeps
its own output tuple and constraints; this slice does not advertise HLS.

The native VOD prerequisite extraction is still a source-only prototype. Its
regression proves that an indexed copy can be checked without attaching a
reader, rendition, preparation session or producer; unresolved encoded
recipes refuse before allocation. Activation, aliases, bounded startup retries
and real-client transport qualification remain subsequent J4 work.

The selector implementation follows the pinned 10.11.11
[ContainerHelper](https://raw.githubusercontent.com/jellyfin/jellyfin/1fbd8739292cce610231be93daf43368733edf63/MediaBrowser.Model/Extensions/ContainerHelper.cs):
a leading minus excludes named codecs/containers, and an empty selector is a
wildcard. A negative codec selector must still apply its required conditions
to every non-excluded source; the regression covers that bypass. Accepting a
protocol wildcard never fills in HDR/DV claims in the native planner.


The profile slice merged through [PR #780](http://forge.lan:3000/noirr/plurx/pulls/780)
as `7a4e56b86071b856703ea357218f75ce229f026a` after all eight jobs passed
on head `687c83602`. Its landing contains the ten checked regression fields.

An independent ancillary/track slice now implements authenticated Intros,
MediaSegments and source-bound subtitle aliases. Intros reports the native
empty pre-roll collection. MediaSegments reuses native persisted/manual marker
authority and chapter fallback; content-derived segment IDs stay stable while
the mapped item/source, kind and source times are unchanged. They are not
mutable catalog entities. The segment-type filter is bounded and closed.
Subtitle requests preserve original global indices and source timestamps,
delegate native VTT extraction/cache, and convert plain compatible cues to SRT
for the observed Infuse format. Unsupported WebVTT styling or timestamp tags
refuse instead of silently changing their meaning. Native extraction failure
and bitmap refusal retain their error responses.

This slice requires authentication on every request and creates no media
capability or playback owner. Full J5 qualification still depends on J4's
native activation and transport work and the deferred client matrix.

On the current integrated base, all 19 focused Jellyfin HTTP regressions pass
(10.27 seconds), both subtitle representation regressions pass, and all 17
documentation/identity/ownership contracts pass. Route deadline and attribution
inventories pass. Workspace/all-target Clippy passes on pinned Rust 1.97.1.
These automated results do not count as physical client qualification.


[PR #781](http://forge.lan:3000/noirr/plurx/pulls/781) merged the ancillary
slice as `e5c6e9d34aad7d382bd4985c055c98ec87dd9131`, with parents
`7a4e56b86` and `fa16201f3`. All eight jobs passed in
[effort run 4023](http://forge.lan:3000/noirr/plurx/actions/runs/4023), and
its landing preserves all seven checked regression references.

The next J4 slice keeps native player identity stable for an authenticated
user/device/client family across login replacement. Direct presence stays
keyed to each play. Binding activation and native pointer activation fence
older compatibility events inside their Store transactions. A pending
negotiation leaves the incumbent active. Private ordering metadata assigned
by the transaction distinguishes pending asks with identical clock readings;
legacy metadata precedes a newly ordered ask. A bound activation nonce makes
a replay unable to terminalize a later negotiation. Neither metadata value
is a producer lease or a public credential.

The supersession slice preserves native source/manual-revision fences and
exact resource references. Late Stop releases only its own direct presence
and grant; it cannot write progress through a superseded binding. Full native
VOD negotiation, activation, HLS aliases and no-signal resource cleanup remain
subsequent J4/J6 work, and this slice does not advertise transcode delivery.


On integrated base `e5c6e9d34`, all 25 Jellyfin Store contracts pass on SQLite
and three voters (253.76 seconds). All 20 focused HTTP regressions and the
stable player identity regression pass. The shared native stale-activation
replay and changed-viewer-request regressions pass. Workspace/all-target
Clippy, all 28 SQL/read/process censuses and all 17 documentation/identity/
ownership contracts pass on pinned Rust 1.97.1. Physical qualification and the
remaining VOD transport work are not included in these results.


The supersession slice landed in PR #782 at `acbce39ee`, with exact parents
`e5c6e9d34` and `a37ade039`. All eight jobs passed in
[effort run 4025](http://forge.lan:3000/noirr/plurx/actions/runs/4025), and
its landing preserves all nine checked regression references.

The next native watch slice admits a ready, leased native incarnation only
through its exact current playback pointer, request fingerprint and original
media clock. Expired and replaced ownership refuse progress. Native cleanup
with the exact `deleted` reason permits final Stop retry when no replacement
pointer exists, retaining the shared atomic final/tombstone transaction.
This does not advertise HLS or complete physical qualification.


On integrated supersession base `acbce39ee`, all 26 Jellyfin Store contracts
pass on SQLite and three voters (258.51 seconds). All 20 Jellyfin HTTP and
shared-progress regressions pass (8.36 seconds). Workspace/all-target Clippy,
all 28 SQL/read/process censuses and all 14 focused documentation/identity/
ownership contracts pass on pinned Rust 1.97.1. These receipts concern native
watch admission; they do not qualify an HLS adapter or a physical client.


The native watch slice landed in PR #783 at `5f27b2905`, with exact parents
`acbce39ee` and `9414e6f86`. All eight jobs passed in
[effort run 4027](http://forge.lan:3000/noirr/plurx/actions/runs/4027),
and the landing preserves both checked regression references.

The next admission slice reserves `jellyfin:<PlaySessionId>` native request
ids and checks their live compatibility binding before native pointer
replacement. Cancelled and missing negotiations refuse without ending the
current native route; a valid replacement and its exact active replay still
work. The replicated insertion repeats admission inside the transaction.
HLS normalization and serving continue separately. Apple TV physical tests
remain deferred and do not block implementation.


On integrated base `5f27b2905`, all 27 Jellyfin Store contracts pass on
SQLite and three voters (270.03 seconds), including revoked replay. Both
ordinary native activation contracts pass. After limiting the new consistent
lookup to replay, its focused admission regression passes again on both
backends (9.82 seconds). Final workspace/all-target Clippy and all 28
SQL/read/process censuses pass on pinned Rust 1.97.1; all 14 focused
documentation/identity/ownership contracts pass. No physical-client or HLS
qualification is claimed by these receipts.


The native admission slice landed in PR #784 at `e8ff688f10`, with exact
parents `5f27b2905` and `18719c68d`. All eight jobs passed in
[effort run 4030](http://forge.lan:3000/noirr/plurx/actions/runs/4030),
and the landing preserves its checked regression reference.

The next transport slice translates one closed transcode tuple at a time,
resolves actual native VOD output before advertising it, and freezes that
recipe fingerprint for activation. Trusted passive/VOD-only policy and the
finite bitrate ceiling cross the existing worker request seam; unknown policy
fields refuse on an older worker. Video rate limits reserve the actual native
AAC budget before applying the native VBR peak. Original source clock remains
zero-based even when the requested start is nonzero.

Mapped master/media/init/fragment/subtitle resources recheck fresh login,
source revision, exact native pointer and publication owner. They delegate the
existing native serving path and its body lifetime. The measured init-prefix
representation maps composite ranges to precise native ranges and rechecks
object validators rather than consuming a whole fragment and slicing it.
The native buffered lifetime wrapper retains known Content-Length.
Constructing the large native handler future in a separate helper keeps the
inline-range adapter within the ordinary regression-test stack.

This slice does not claim full J4/J6 completion: shared bounded startup retry,
encoded/HEVC service qualification, no-signal and replacement cleanup, and the
physical requesting-client matrix remain open. Apple TV use stays deferred.
Generated media URLs contain no login token and every media request remains
authenticated; no anonymous artwork or new media bearer capability is added.


On current admission base `e8ff688f10`, all 26 focused Jellyfin HTTP tests
pass (9.68 seconds), including real indexed copy/range/original-clock/Stop
cleanup on the ordinary stack. All 22 pure wire tests, eight compatibility
service regressions, the finite worker-budget regression, and the existing
native range/frontier and trace-redaction regressions pass. Workspace/all-target
Clippy, all 28 SQL/read/process censuses and all 17 documentation/API/identity/
ownership contracts pass on pinned Rust 1.97.1. These are service receipts;
encoded/HEVC and physical-client qualification are still open.


The native HLS slice landed in PR #785 at `d373716f4`, preserving all sixteen
checked regression references. All eight jobs passed in
[effort run 4032](http://forge.lan:3000/noirr/plurx/actions/runs/4032).

The shared-start slice waits on the exact native incarnation already claimed
by the same request, then accepts its exact active compatibility binding when
another waiter publishes first. A duplicate waiter cannot release that
canonical session merely because it lost the binding update. Transient native
startup/capacity/owner-transition errors retry under one fifteen-second create
deadline, with the remaining allowance propagated into native create. Index,
source-change and unknown failures remain terminal; native private error
details are replaced by a public message while preserving typed status/code.
All waits belong to the calling request; no task, media owner or producer is
added.

The real native copy regression now exercises two simultaneous master entries,
one live/preparing session, precise inline ranges, original-clock watch
progress and Stop cleanup. Cancellation between native publication and
compatibility binding, no-signal cleanup, encoded/HEVC service qualification
and the physical client matrix remain open. Apple TV testing stays deferred.


The shared-start slice landed in PR #786 at `235e1c44d`, with exact parents
`d373716f4` and `111c24b8d` and the same tree as the tested head. All eight
jobs passed in [effort run 4034](http://forge.lan:3000/noirr/plurx/actions/runs/4034);
the landing preserves all three checked regression references. All 28 local
Jellyfin service tests, 17 documentation/API/identity/ownership contracts,
workspace/all-target Clippy and the normal commit hooks passed on Rust 1.97.1.

The next lifecycle slice saves the exact compatibility cleanup reference
inside the native publication transaction. Publication rechecks its reserved
request, current native pointer, fingerprint, original clock, scoped login/token
and mapped IDs. A play cancelled before publication cannot publish; a response
lost after committed publication still leaves Stop its exact native reference.
Exact replay retains the first activation nonce and cannot fence a newer
pending ask. Existing native publication/release and unknown-commit owners
remain responsible for cleanup; no additional media owner, task or consistent
read is introduced.

The source-only regression first reproduced a published native route with a
still-pending compatibility row. With atomic binding, all 17 compatibility
storage contracts pass on SQLite and three voters, including publication,
cancellation, replay and pending limits. The ordinary native activation/
publication/replay contract, all 28 Jellyfin HTTP regressions, workspace Clippy
and all 28 SQL/read/process censuses pass. The exact integrated branch is
rechecked before pushing. No-signal/replacement cleanup, encoded/HEVC service
qualification and the physical client matrix remain open; Apple TV stays
deferred.

PR #787 merged as `b138ae619` with parents `235e1c44d` and `1a2b6499d`, the
tested head's exact tree and its three checked regression references, after
all eight jobs passed in
[effort run 4039](http://forge.lan:3000/noirr/plurx/actions/runs/4039).

## 11. J4 completion — access, lifecycle and the remaining service matrix

This slice finishes the service-side J4 work the handoff left open. Every
change sits on an owner that already existed; it adds no timer, background
task, playback owner or producer.

**Encoded HLS qualification.** The real native HLS fixture also runs the
encoded path: a complete probe, an asserted-absent copy index, copy refused by
the request and a 750 kbit/s ceiling frozen into the recipe. A fetched mapped
fragment decodes for both video and audio through a finite, awaited test
decoder, which the ownership inventory records.

**Approved access (Paul, 2026-10-03).** Mapped Primary/Backdrop artwork answers
without a login while the switch is on, keeping the per-address miss budget and
the existing artwork owner. Direct play has a scoped link: Jellyfin Android TV
0.19.10 builds its own direct URL from `mediaSource.eTag` and sends neither a
credential nor a `PlaySessionId` (checked in its pinned source), so the
negotiation returns one 256-bit secret as the source ETag. The secret is the
token of the native one-file grant minted with the negotiation, expiring
within 24 hours of it. A grant-keyed read resolves its play; Stop,
supersession, logout, token revocation, idle expiry and an earlier switch
generation all end it (typed `410 media_link_gone`). The direct route also
parses Jellyfin's case-insensitive query names; before this it required a
case-exact `PlaySessionId` that Android never sends.

**Lifecycle root causes.**

| Gap | Cause | Fix on the existing seam |
|---|---|---|
| Long pause returns 410 | Paused progress never reached the native passive grant, so it expired 600 s after the last media request | Authenticated Progress renews the exact grant (user, player, incarnation) on whichever node owns the route, through a new relay resource beside Status and Delete |
| Old session left after a replacement without Stopped | Activation ended predecessor bindings but nothing released their native session or direct grant | Supersession records `superseded_by`; the activating request reads that bounded set and releases each through the native owner |
| Active bindings accumulate | A vanished client's active row had no end condition | Admission retires active rows whose native session has been terminal, or whose grant expired or was revoked, for the full 24 h tombstone window, then terminal retention deletes them |
| Logout extended tombstones | `END_LOGIN` rewrote every scoped row's expiry | Ended rows keep their expiry; they are still returned for idempotent release |
| Switch off raced in-flight activation | Enablement was checked once at request entry | Each play records its switch generation; admission, activation, native admission and publication commit only while that generation is saved and enabled. Off ends a play for good |
| Pointer supersession could pick the wrong ask | Chosen pending ask was "the only one with this fingerprint" | The native activation's exact reserved request names the chosen play |
| Infuse direct events refused | Infuse labels static Range delivery `DirectStream`; the adapter required `DirectPlay` | Refuse only a label naming the other delivery |

**Closed manifests and representation.** The manifest rewriter now refuses an
unknown colon-less `#EXT` tag (native playlists emit only `#EXTM3U`,
`#EXT-X-ENDLIST`, `#EXT-X-DISCONTINUITY` and `#EXT-X-INDEPENDENT-SEGMENTS`) and
a subtitle playlist naming another track's cue. A direct representation test
pins native truth through the facade: 416 with `bytes */len`, suffix ranges,
no invented validator, `If-Range` serving the whole file, and HEAD.

**Pre-promotion review fixes.** Three scoped reviewers read the trial merge
of current `main` into the effort (shared native seams, facade security, play
lifecycle). What they found and what changed:

| Finding | Root cause | Fix |
|---|---|---|
| A compatibility login was a full native bearer, including admin | The login row is an ordinary `tokens` row and native authentication never asked which surface it was issued for | Token audience: native, Plex and cached admin proofs refuse compatibility logins in the same authentication statement; the facade accepts only them |
| Sign-out could leave the token valid | Logout released media before revoking, and a busy release returned 503 first | Revoke first; release best-effort afterwards |
| Old-generation HLS kept serving after off/on, and its Stop was refused | HLS resources skipped the generation check that Stop enforced | Resources check the generation; Stop ends and releases a play from any generation without writing progress |
| Stop could leave a just-published native session | Release used the pre-END snapshot | Release re-reads the row END produced |
| A renegotiation left the abandoned direct grant live until expiry | The pending play was ended but not released | Released with it |
| Real clients would hit 429 on poster grids, and one IPv6 host could fill the table | The per-address budget counted warm hits and keyed full IPv6 addresses | Only misses spend budget; IPv6 counts per /64 |
| A direct play whose grant expired overnight lost its final Stop | The retirement window applied to revoked grants only | Expired grants keep the same 24 h window |
| Tombstones grew at a login's request rate | Only the 24 h TTL bounded ended rows | Admission keeps a login's newest 256 tombstones |

A Fable 5.1 review of the main-merged candidate verified every fix above and
found five more, all fixed:

| Finding | Root cause | Fix |
|---|---|---|
| A renegotiation could end an incumbent that activated during startup and strand its native session | The Rust `pending` check was not in the SQL, and release used the pre-END snapshot | `withdraw_pending_jellyfin_play` ends only a still-pending ask, so the snapshot it releases is exact |
| A standalone node crashing in v95's commit window could never start again | v95 lacked the restartable `ADD COLUMN` guard v41–v90 carry | The guard checks the revision column (superseded: main's `apply_migration_step` now commits each step's marker with its DDL, so the step is v101 and needs no guard) |
| `/api/v1/grants/{token}/content` answered a Jellyfin link (a validity oracle) | Links share the native one-file grant table | The open-in route refuses any grant a Jellyfin play owns before saying whether it is live |
| A failed VOD bind stranded the native start until idle expiry | Only the refused path released | A start no binding claims is released; a claimed or unknowable one is left to its owner |
| Logout re-released every retained tombstone | Release ran for already-ended native sessions | Release skips a native route that is already ended |

Lower findings kept as documented behavior: using a link counts as activity
for its login (bounded by the link's 24 h life); a direct grant is minted
before the pending cap refuses its play and is then revoked; a response already
streaming finishes after the switch is saved off.

Recorded as design choices rather than defects: a manual mark now bumps every
row's manual revision even when unchanged (the compatibility fence relies on
an external no-op mark); a native beat labelled `transcode` no longer touches
direct-play presence; a failed immediate progress write keeps the older queued
beat. A same-login manual edit made between `PlaybackInfo` and the first media
request still fences that one play's progress; the window is seconds long.

Still open: the physical Infuse and Android matrix on the frozen candidate,
HDR/Dolby Vision, the multipage corpus, and the effort's promotion.

### Acceptance run and promotion review

A disposable real-binary server (hiqlite store, a 12-minute film, real HTTP)
ran the direct, link and HLS flows. Every direct-play and link check passed.
It found two HLS defects, fixed in #796:
- the master alias answered a media playlist, which Infuse refuses;
- a client resuming after a long pause overflowed the worker stack. A gdb
  frame walk measured 643 KB in the facade's `serve` frame, because each
  arm's native future was built on the stack before boxing. Construction now
  happens outside the polling frame. The stack size is unchanged.

The Fable 5.1 review of the promotion candidate then found that the
always-multivariant master advertised the file's text renditions even for a
play negotiated without manifest subtitles (Infuse's profile, or a burn-in).
That master now carries no subtitle group, and that play's HLS subtitle
resources answer 404.

**Merged with the architecture review (#793).** Main took SQLite v92–v97 and
replicated v70–v73, so the Jellyfin steps are now SQLite v98–v101 and
replicated v74–v77. The private-lineage bridge stamps the end of the union it
builds (v73), and the Jellyfin steps run after it.

Main's `apply_migration_step` commits each SQLite step's marker with its DDL,
so the v95 crash-restart guard is gone. Fixtures that rewind a marker drop the
watch columns instead, as main does for its own `ADD COLUMN` steps.

Jellyfin plays carry no client network identity: both negotiation and native
activation run with no network prior. So they agree, and these plays take no
part in priors or candidate recovery.

Open, rare: on an SDR HEVC High-tier copy with no text tracks, the native
master still serves its direct media-playlist envelope, because Apple's
eligibility check rejects High-tier declarations in a multivariant. Infuse
would see a media playlist at the master URL for that file. Negotiation
already refuses HDR copy, so it reaches only that source class; J6 decides it
with the physical client.
