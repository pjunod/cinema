# Split HLS as the default — inventory, client matrix and baseline method

**Status:** open — S1 of P62 (metrics, first copy-HLS baseline) merged; S2
([contract](SPLIT-HLS-RENDITION-CONTRACT.md)) built; S3 in verification; S4–S8 pending ·
**Owner:** Claude Opus 5.5 · **Effort:** `E-5F2C2E9EA4` ·
**Written:** 2026-11-26 against `main` @ `8724851ba`

Goal: every supported HLS surface plays directly published, separate video and
audio renditions on one film timeline by default, with an explicit, working
muxed fallback for incompatible clients, unsupported inputs, startup refusal
and recovery. Muxed HLS stays supported, and direct play stays direct play.
This document is the starting point. It covers what exists (§1), the gaps
(§2), the matrix (§3), the slices (§4) and how the baseline is measured (§5).
Every `file:line` is from `8724851ba`; re-verify by function name.

`P/` = `crates/plurxd/src`, `C/` = `crates/plurx-core/src`.

## 1. What ships today

Three split-shaped layouts exist, and they share no contract.

| Layout | Where | Video | Audio | Admitted when |
|---|---|---|---|---|
| P49 split copy family ([plan](SHARED-VIDEO-MULTILINGUAL-VOD.md)) | `C/transcode/vod.rs` `VodSplitCopyFamily`, `P/vod/serve/split.rs` | Indexed copy with `-an` | One AAC re-encode per soundtrack (`VodSharedAudioRecipe`, 3 CPU threads each) | The web hls.js player sends `split_audio`, and the title is an AVC copy without DV on VOD with 2–3 soundtracks, no burn (`P/transcode/manager/create.rs` `prepare_vod_split_soundtracks`) |
| Continuous-quality family | `C/transcode/vod.rs` `VodPresentationFamily` | Encoded video-only AVC rungs | One shared AAC | Web hls.js or the Android continuous extractor |
| Native Safari FEL split ([build](FEL-SERVER-SPLICE-BUILD.md)) | `P/copyseg.rs` `NativeSplitAuthor`, `P/http/hls/playlist_text.rs` `native_split_master_text` | Projected out of the copy mux | Projected out of the same mux, with no extra encode | `transport == "native"` with caps `hev1`, P7 FEL→8.1, many decline reasons (`P/transcode/manager/start.rs`, `P/copyseg.rs`) |

Everything else is muxed:
- VOD copy with 1 or ≥4 soundtracks, and HEVC/DV copy outside the native FEL path.
- VOD transcode and processed DV windows.
- Rolling copy, and rolling transcode (MPEG-TS, `C/transcode/mod.rs` `hls_args`).
- Live TV (TS or fMP4, hand-written master in `P/http/live_tv.rs`), and DVR (VOD of a file).
- Sharing, offline packages and Jellyfin.

## 2. Gaps a split default has to close

1. **Capability.** Caps v2 (`C/playback/caps.rs` `DeviceCaps`) has no split
   or alternate-audio field. The only signal is the create flag `split_audio`,
   which only the web hls.js copy path sends. `transports` already carries
   tokens such as `hls-bounded-range-v1`.
2. **Audio cost.** Per-soundtrack AAC re-encoding bounds P49 by encoder
   capacity (`vod_family_capacity`, at most 3 roles). The native split shows
   the other way: project the audio out of the copy it already produces. A
   default has to copy or project audio the client can decode, and encode AAC
   only when it cannot.
3. **One contract.** There are three master generators, three child naming
   schemes and three identity schemes. P49 uses
   `audio/{rendition_key}`. Continuous uses `GROUP-ID="shared"`. Native uses
   `video|audio/{sha256(init)}` with `VIDEO-RANGE=PQ` hard-coded.
4. **Rolling and recovery.** Takeover disables the segmenter and the native
   split, and VOD refuses takeover (`vod_reopen_required`). No path carries
   split state through takeover or cross-node failover. Seek and reopen
   decline the native split.
5. **Cross-node wire.** Media-pool protocol 12. Offers carry the literal
   `output_contract: "hls-mpegts-v1"` (`P/media_pool.rs`). A split contract
   needs a protocol bump and a dated rolling-upgrade note in
   [OPERATIONS](../OPERATIONS.md).
6. **Closed grammars.** The sharing grammar accepts only subtitle
   `EXT-X-MEDIA` (`C/sharing_resources.rs`), and the shared receivers pin
   `audio_alternates = false`. Jellyfin `rewrite_manifest`
   (`crates/plurx-compat-jellyfin/src/hls.rs`) rejects audio `EXT-X-MEDIA`
   and role URIs.
7. **Clients.** See §3.
8. **Unchanged by design.**
   - Direct play on every client.
   - The progressive `/stream.mp4` remux.
   - Plex, which is direct only.
   - Offline downloads, which stay one muxed variant
     ([offline plan](../clients/OFFLINE-VIEWING-PLAN.md)).
9. **Measurement.** Before S1 there was no per-object HLS request count, no
   per-role byte count and no running stored-versus-produced ratio, and no
   committed muxed baseline beyond P49's ledger (§5).

## 3. Client × surface × layout matrix

The layout cell says what is served today, and what the client would need to
play a split presentation.

| Client | Transport | Split today | Audio switch | Needs for a split default |
|---|---|---|---|---|
| Web, hls.js (MSE) | `hlsjs` | P49 copy (2–3 AVC soundtracks); continuous family | In place via `hls.audioTrack` | Capability token instead of `split_audio`; the rest already plays groups |
| Web, Safari native HLS | `native` | Native FEL split only (Safari 27.2 Mac UA) | Session recreate | `nativeHlsPlaylists` (`P/web/player/player.js`) accepts exactly one `STREAM-INF` and ignores audio groups |
| Apple app (iOS, tvOS) | AVPlayer | None (no `split_audio`, no `transport`) | Session recreate (`selectAudio` → `reopen`) | Token; `HLSFragmentCadence`, `GrowingPlaylistPreparation` and `autoVODAdvertisedIntervals` parse single-variant masters only; `.audible` in-place selection on HLS |
| Android (Media3) | ExoPlayer | Continuous family through a custom extractor | Session recreate (`switchAudio` → `restartAt`) | Token; `TrackSelectionOverride` on HLS audio groups |
| Linux player (mpv) | `mpv` | None | Session recreate | Unverified whether mpv's HLS demuxer plays fMP4 audio groups; verify, then declare |
| Desktop remote | — | — | — | No media; out of scope |
| AirPlay | Native HLS only | Follows the native path | — | Native path support |
| Offline (Apple, Android) | Download | Muxed, by plan | — | Stays muxed |
| Jellyfin clients (Infuse, Android TV) | Compat HLS | Muxed; grammar rejects groups | — | Grammar change or explicit muxed per client |
| Plex / Kodi | Direct play | — | — | Unaffected |

| Surface | Producer today | Split candidate |
|---|---|---|
| VOD copy | Indexed fMP4 copy, muxed or P49 | S3 |
| VOD transcode, processed DV windows | Encoded fMP4, muxed | S4 |
| Rolling copy | `copyseg` fMP4, or legacy ffmpeg HLS on takeover | S5 |
| Rolling transcode | ffmpeg HLS MPEG-TS | S5 (fMP4 where split) |
| Rolling recovery, reopen, takeover | Splits disabled | S5 |
| Live TV | ffmpeg HLS, TS or fMP4 | S6 |
| DVR | VOD of the recording | With S3/S4 |
| Shared playback | Relayed Source sessions, closed grammar | S6 |

## 4. Slices

Each slice is one batched pull request into `main`.

| # | Slice | Acceptance |
|---|---|---|
| S1 | Metrics and muxed baselines (§5), this document | Baseline half of the capacity criterion; the matrix |
| S2 | One rendition contract and one writer for every split master; caps v2 token `hls-split-av-v1` with `split_audio: false` as the muxed opt-out; one split eligibility predicate; protocol 13: a split copy offer names its audio-free video ([contract](SPLIT-HLS-RENDITION-CONTRACT.md)) | Contract, identity |
| S3 | VOD copy split by default: audio copied or projected when decodable, AAC otherwise; AC-3/E-AC-3 init facts; audio cuts on the packet nearest each video boundary (moved from S2); lifts the 2–3 limit; atomic group publication; muxed admission fallback before exposure | Capacity, identity |
| S4 | VOD transcode and processed DV/HDR windows on the contract | Contract |
| S5 | Rolling copy and transcode, recovery, reopen, takeover, cross-node relay; seek and reopen onto the native splice; stop writing the native split's muxed copy ([readers](NATIVE-SPLIT-MUXED-READERS.md)) | Identity, recovery; the native split's write-saving acceptance ([§3](NATIVE-SPLIT-MUXED-READERS.md#3-what-the-split-only-change-has-to-do)) |
| S6 | Live TV, sharing and Jellyfin | Matrix |
| S7 | Apple, Android, web native Safari and Linux clients; diagnostics name the audio rendition | Player matrix |
| S8 | Qualification on physical devices, failure paths, rolling upgrade, capacity against S1, then the default | All |

S2 and S3 started after P49's pull request #1888 landed (2026-11-27), because
it changed the split video key.

## 5. How the baseline is measured

**Series added in S1.**

| Series | Labels | Answers |
|---|---|---|
| `plurx_hls_requests_total` | `surface` (session, live_tv, offline), `resource` (master, media_playlist, init, segment), `role` (none, muxed, video, audio, subtitle), `status` (200, 206, 304, 4xx, 5xx, other) | Requests a presentation costs per object class, revalidations and ranged resumes |
| `plurx_hls_response_bytes_total` | `surface`, `resource`, `role` | Bandwidth per rendition role |
| `plurx_vod_segment_answers_total` | `source` (stored, produced) | Running cache reuse of VOD segments: stored bytes versus bytes a GET waited for |

The labels come from the matched route template, plus path values the routes
accept from a fixed vocabulary, so no session, rendition or object name can
become a label (`P/http/hls_request_metrics.rs`). Only GETs are counted.

Two caveats:
- For a relayed session, the HLS series are counted on the edge node and the
  segment answers on the owner. A cluster reading must sum both nodes; a
  single-node lab reading sees both.
- A `stored` answer is counted when the bytes are opened. In the rare case
  that delivery then refuses them (a cached child over its delivery budget),
  it is still counted. This overstates reuse by those refusals only.

**Existing series used with them.**
- `plurx_create_seconds` and `plurx_start_outcomes_total`: startup.
- `plurx_vod_producer_generations_total`: producer spawns.
- `plurx_vod_gets_serving` and `plurx_vod_blocked_gets_*`.
- The playback cache and scratch ledger gauges.
- The playback lab's client timings ([PLAYBACK-TESTING](../PLAYBACK-TESTING.md)).
- The benchmark sampler's CPU, RSS and network columns
  ([BENCHMARKING](../BENCHMARKING.md)).

**Method.** For each representative title, take a `/metrics` reading before
and after one scripted play: start, 60 s, one seek, one audio change, stop.
Use the same client and the same title in muxed and (once it exists) split
layout. Record the deltas, the first-frame time, the scratch high-water mark
and the CPU seconds of the producers.

Representative titles:
- 1080p AVC with one AAC track.
- 1080p AVC with three tracks, including AC-3.
- 4K HEVC HDR10 with E-AC-3.
- DV Profile 8.1, and DV P7 FEL.
- A Live TV channel.

Results go under `docs/performance/evidence/` as JSON. The split default (S8)
must meet or beat each figure, or the shortfall is a recorded decision.

**First reading (2026-11-26).** The data is in
[split-hls-baseline-20261126.json](../performance/evidence/split-hls-baseline-20261126.json).
It is copy-HLS VOD in desktop Chrome on a debug build. Transcode is not
measured yet, because the lab host lacks the pinned media runtime.

| Same 1080p AVC title, steady 60 s | HLS requests | HLS bytes | Producers |
|---|---|---|---|
| Muxed copy | 8 (1 master, 1 playlist, 1 init, 5 segments) | 22.77 MB | 1 copy |
| P49 split | 21 (1 master, 2 playlists, 2 inits, 5 video + 11 audio segments) | 22.73 MB | 1 copy + 2 AAC encodes |

The bytes are equal. The split costs about 2.6× the requests, because its AAC
segments are about 2 s against copy video segments of up to 6 s, and it adds
an encoder per soundtrack. S2's contract should align audio segment
boundaries with the video plan; it is a producer change, so S3 carries it with
the audio copy or projection that removes the encoders. Both claims are checked
against this reading.
