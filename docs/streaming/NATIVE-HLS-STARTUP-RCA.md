# reference film G startup — native HLS mistakes an unavailable playlist for a codec failure

**Status:** diagnosed; Fable review incorporated; implementation handoff ready ·
**Written:** 2026-09-17 UTC (2026-09-16 Eastern).
**Review:** Fable approved with changes. The dispositions below supersede
the original proposal; product changes remain unimplemented in this task.

Companion to [the HLS startup status](WEB-HLS-STARTUP-RECOVERY-STATUS.md)
and [the freeze recovery repair](WEB-PLAYBACK-FREEZE-RECOVERY-IMPLEMENTATION.md).
This investigation covers reference film G's failed Safari startup on nynuc. It extends
startup recovery to the native HLS transport, which the deployed hls.js
startup controller does not cover. Build instructions now live in the
[implementation handoff](NATIVE-HLS-STARTUP-IMPLEMENTATION.md).

## 1. Finding — a temporary preparation failure became a terminal refusal

The confirmed application root cause is missing native HLS startup recovery
and premature decoder attribution. The server answered the initial master
playlist request with HTTP 503 `response_publication_timeout`. Safari
reported media error 4. The web handler interpreted that alone as a codec
rejection and replaced a still-preparing copy session with a transcode.
The transcode's source verification then timed out, and the server classified
that inability to inspect the source as `vod_source_rescan_required`.

The original copy producer subsequently generated media. Retrying the same
file, position and copy/Dolby Vision recipe succeeded. The failure does not
establish either an unsupported codec or a changed source requiring rescan.

The encoded-source error is an independent defect: `prepare_vod_encoding`
returns before the held-source probe for copy without burn. Correct native
readiness would have retained reference film G's copy/DV session without invoking that
probe. The false rescan verdict still needs repair for transcode/burn starts
on web, Apple and Android, including errors from `open_source_fence`.

**The initiating delay has a narrower evidentiary limit.** The media-origin
probe exceeded five seconds, and initialization remained unavailable through
two five-second publication deadlines. The file is on QNAP NFS. Later exact
probes took 58 ms for source verification and 117 ms for the resume seek.
Transient source/storage latency is the leading explanation. No retained
syscall trace or NAS event establishes disk spin-up, a slow NFS read, or
another specific storage mechanism. Do not present one as proved.

## 2. Evidence — deployed source and correlated attempts

The running build was `v0.3.0-2633-gc9e4edf4`, commit
`c9e4edf451e12247a7aa4188903e5ba36888e7e9`. Source was read using
`git archive` from nynuc's deployment checkout. The local documentation
checkout is older and has unrelated edits; its player is not the incident
implementation. Symbol names below refer to the deployed source and must be
rechecked against the intended implementation base. Fable reviewed main
`363a22e28aa53094d899a9ad3c812241a6243548`; this revision independently
fetched that base and confirmed all five incident source files are
byte-identical to the deployed commit.

| Source location | Incident symbol / deployed line | Review purpose |
|---|---|---|
| [Web player](../../crates/plurxd/src/web/index.html) | `attachHls`, 7245; native branch, 7596 | Startup episode exists only on the hls.js branch |
| [Web player](../../crates/plurxd/src/web/index.html) | `wirePlayerMedia`, 12992 | Native media error reports failure and may transcode before checking server evidence |
| [Web player](../../crates/plurxd/src/web/index.html) | `refreshSegTimes`, 10788 | Native playlist HTTP failures are discarded |
| [Playback policy](../../crates/plurxd/src/web/playback-policy.js) | `fallbackAction`, 636; `HLS_STARTUP`, 926 | Codec fallback predicate and existing startup limits |
| [Source probes](../../crates/plurxd/src/ffmpeg.rs) | `held_source_probe_json`, 358; `bounded_command_output`, 971 | Remote media inspection inherits the engine timeout |
| [Session preparation](../../crates/plurxd/src/transcode.rs) | held-source probe mapping, 17282 | Any inspection error becomes a rescan refusal |
| [HLS HTTP handlers](../../crates/plurxd/src/http/hls.rs) | `session_start_error`; `response_publication_timeout`, 9178 | Typed HTTP status/code mapping |

These links open this checkout's files for navigation. The deployed line
numbers are evidence locators, not claims that the older local files contain
the same implementation.

The title is file `120`, an 82,911,217,396-byte MKV with 2160p HEVC Dolby
Vision Profile 7, TrueHD primary audio, 36 tracks and 20 chapters. The resume
position was `4116.803` seconds. Both attempts used rolling copy HLS because
the immutable VOD index was pending. Video was copied with Dolby Vision
conversion to Profile 8.1, and audio was converted to AAC.

| UTC on September 17 | Observed event | Consequence |
|---|---|---|
| 00:52:28.661 | Copy recipe arguments logged | This precedes the awaited media-origin probe; it is not proof FFmpeg already spawned |
| 00:52:33.663 | Media-origin probe times out; prepublication copy session starts | Probe consumed its full five-second budget |
| 00:52:39.096 | Master request returns 503 after 5,001 ms waiting for init | Server says publication is temporarily unavailable |
| 00:52:39.168 | Safari reports code 4; application chooses transcode | No server-response verification precedes fallback |
| 00:52:44.124 | Another master request times out after five seconds | Init is still unavailable |
| 00:52:45.326 | Transcode creation fails after held-source probe timeout | Timeout is returned as `vod_source_rescan_required` |
| 00:52:47.889 | Original producer logs Dolby Vision conversion | Producer progressed after the client had refused playback |
| 00:52:57.420 | Original producer holds with 80 seconds ahead | It produced substantial media using the original recipe |
| 00:53:12.536 | Idle lease reaper retires original session | Later final-segment ENOENT is cleanup aftermath |
| 00:55:50.901–50.993 | Same resume recipe; origin probe/session preparation completes in about 92 ms | Slow startup was not reproduced on retry |
| 00:55:51.097 | Retry producer logs Dolby Vision conversion | About 0.2 seconds after recipe logging |
| 00:56:03.561 | Safari reports first frame, 15,363 ms after open | Same 2160p copy recipe plays; user confirms success |

The second master GET is unattributed: it may be the application playlist
fetch or native Safari work. The retained logs lack request-origin and
connection-lifetime evidence to choose between those explanations. Its
timeout does not prove either duplicate application dispatch or a server
cancellation leak.

Read-only checks found no kernel messages in the incident window. The
available sysstat sample ending at 00:50:03 predates the failure; its idle
CPU and zero NFS retransmissions cannot rule out a later short stall.
Cumulative NFS counters cannot attribute latency to this attempt. QNAP SSH
authentication was denied, so NAS-side events were unavailable.

Bounded probes used the deployed `/usr/lib/jellyfin-ffmpeg/ffprobe`, including
the exact descriptor-bound `-show_format -show_streams -show_chapters`
verification and the four-packet seek at `4116.803`. They changed no media,
cache objects, server settings or running sessions. No cold-cache eviction,
NAS wake/sleep experiment, server restart or deployment was performed.

## 3. Code — three decisions turn waiting into failure

**Native HLS bypasses the startup controller.** In `attachHls`, the
`hlsStartup` episode, manifest-aware retries and typed HTTP observation are
created inside the hls.js branch. The native branch starts `refreshSegTimes`,
assigns the playlist to the video element and applies playback intent.
`refreshSegTimes` silently returns on non-OK responses; it supplies no failure
evidence to recovery. Thus its application-visible HTTP response is lost.

**Media error 4 unconditionally qualifies for decoder fallback.**
`wirePlayerMedia` passes `mediaFailure: code===3||code===4` to
`PlaybackPolicy.fallbackAction`. Before first presentation, remux plus code 4
immediately invokes `startTranscodeFallback`. There is no HTTP probe or typed
failure classification on this path. In this incident the native error
arrived 72 ms after the server's 503.

**Source inspection failure is conflated with mismatch.** Encoded VOD
preparation calls `held_source_probe_json`. That helper shares the five-second
`ENGINE_PROBE_TIMEOUT` with local engine introspection commands. The caller
maps every helper error to `vod_source_rescan_required`, before comparing
the fresh probe with the stored probe. A timeout therefore receives the
same terminal action as an established metadata mismatch. The earlier
`open_source_fence` error mapping has the same defect, including open/stat
I/O failures; the correction must cover both boundaries.

A Node VM replay executes the exact deployed `wirePlayerMedia` function and
real deployed playback policy, with DOM and side effects stubbed. Code 4
invokes transcode without a `fetch`; code 2 stops without transcode. This
reproduces the application's decision, not Safari's internals. The production
logs establish Safari's observed error code and correlated server response.

The [replay script](../evidence/native-startup-replay.cjs) accepts an
extraction of the incident archive:

```bash
node docs/evidence/native-startup-replay.cjs /path/to/incident-source
```

Expected incident output is `CONFIRMED` with code 4 using transcode and zero
HTTP checks. That output confirms the defect; it is not a passing regression
criterion for the repaired player. Its fetch stub intentionally throws;
new inverse-behavior tests with controllable fetches must fail on the old
wiring and pass on the repair. A failure at a source-hash guard alone does
not establish mutation sensitivity of the decision assertion.

## 4. Accepted fix — distinct tracks with explicit ownership

The [implementation handoff](NATIVE-HLS-STARTUP-IMPLEMENTATION.md)
is the build contract. It replaces the original proposal's unresolved
budget choices and decoder-evidence questions.

1. **Native readiness before source assignment.** The existing startup
   episode owns sequential master and selected child playlist preflight.
   The native video element receives no source until both are usable. Typed
   temporary failures preserve the same session and copy/DV recipe. Use an
   explicit transport/attachment identity, not `null===null` HLS identity.
2. **One post-readiness classification and reload.** Only errors after source
   assignment enter the native error ladder. Verify current master, child
   and init readiness; perform one same-session reload; then permit the
   existing one compatible fallback for persistent rejection. With no
   decoder-facing signal, attribute fallback as unverified refusal before
   decode. A successful resource fetch is not proof of decode.
3. **Clear the sticky client override.** Same-session reload clears the old
   `controlRenderOverride='failed'`. Move the failed report to terminal or
   compatible-fallback branches. At the reviewed base the server does not
   act on `RenderState::Failed`; it did not retire the incident producer.
4. **Source inspection becomes joinable independently.** Choose Fable's
   option 2(b): a five-second HTTP wait may report not-yet while one supervised
   source preparation continues under the same request identity and fixed
   deadline. Do not kill and relaunch a probe on each response retry. Typed
   open/stat/probe failures stay distinct from demonstrated mismatch.
5. **Keep phase budgets and start-only create retry.** Web creation stays
   20 seconds; attachment stays 40 seconds cold / 20 seconds seek. Maximum
   time for one create-plus-attachment cycle is 60 / 40 seconds respectively.
   Pauses and reloads renew neither clock. A replacement timeout refuses the
   change and preserves its valid predecessor; do not widen the existing
   start-only retry row into an unconditional stop path.
6. **Coordinate all three clients.** The new source timeout code must be
   added to the shared surface fixture and web, Kotlin and Swift policies,
   with terminal expiry kept distinct. Native drift tests and compilation
   belong in acceptance, not a later compatibility follow-up.

The dedicated 15-second FFprobe proposal is withdrawn. Inside a 20-second
create owner it gives one substantive attempt, then at most four seconds
for the second; it does not implement the joinable recovery above. The
handoff specifies separate response-wait and job-lifetime clocks, retained
claims, stale-completion fencing, bounded admission and cleanup ownership.

`refreshSegTimes` currently reads a master with no `EXTINF` when native
subtitle renditions are present. Its `segTimes`/boundary markers remain
empty there. Use the selected child media playlist for those times when
coalescing readiness reads; this is separate from real buffered ranges.

Code 4 currently does not teach a persistent decode limit:
`rememberDecodeLimit` belongs to margin rescue. Its current exposure is
incorrect `causeIsDecode=true` reporting. The repair fixes attribution and
preserves non-learning for ambiguous refusal.

## 5. Fable review — disposition of every finding

| Finding | Disposition |
|---|---|
| 1: source probe is off the intended copy path; source open also wrong | Accepted; independent Track S covers open/stat and probe errors |
| 2: proposed probe cap makes retry ineffective | Accepted; selected joinable option 2(b), rejected kill-and-relaunch option |
| 3: new create code affects all three clients | Accepted; fixture, web/Kotlin/Swift restatements, drift tests and native compile evidence mandatory |
| 4: pre-src readiness, explicit native identity, sticky failed override | Accepted; all binding in Track N |
| 5: document gate depended on untracked files and index edits | Accepted; committed STATUS link, focused index additions, clean-main validation |
| 6: native master does not populate segment times | Accepted; child media playlist supplies timing |
| 7: second master request origin unknown | Retained as unresolved evidence, not guessed |
| 8: failed render report is server-inert | Corrected causal statement; client override remains relevant |
| 9: attribution rather than existing code-4 learning | Corrected scope; margin-rescue learning remains untouched |

Fable's answers about causality, pre-src ownership, finite phase budgets,
credential preservation, change-context safety and inverse tests are
incorporated in the handoff. The source-job lifetime extension still needs
normal code review when implemented; this document does not treat existing
`InFlight → Wait` plumbing as sufficient by itself to make detached work safe.

## 6. Validation — distinguish historical evidence from implementation proof

Completed investigation evidence: the hash-pinned deployed-code VM replay
confirms immediate code-4 fallback; exact production held-source and seek
probes completed in 58 ms and 117 ms; production retry reached first frame
with the same recipe and the user confirmed playback.

The original “four documentation tests passed” result came from the dirty,
older worktree. It was insufficient evidence for main: an untracked companion
and local index edits made links resolve there. The revised deliverable uses
only committed main companions and focused new index rows. Validation is
performed in a clean archive of main
`363a22e28aa53094d899a9ad3c812241a6243548`, with only the RCA, implementation
handoff, replay script and their index rows overlaid and added to that
snapshot's index so the tests include every deliverable.

Run:

```bash
python3 -m unittest discover -s tests/operations -p test_docs_index.py
node docs/evidence/native-startup-replay.cjs /path/to/incident-source
```

The four documentation checks pass on that composed snapshot. The replay
continues to confirm the historical defect. No repaired-player tests, Rust
builds or native Safari fault-injection results are claimed. Follow the
handoff for actual implementation validation and repository qualification.

No source, service, mount, cache or media change was made by this
investigation. The NAS cause remains unproved.
