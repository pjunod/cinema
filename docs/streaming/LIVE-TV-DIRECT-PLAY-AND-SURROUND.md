# Live TV — direct play first, and 5.1 stays 5.1

**Status:** built · **Written:** 2026-09-24 · **Branch:** `fix/live-tv-direct-play`

Companion to [Live TV features](../FEATURES.md#4a-live-tv--the-antenna-on-every-screen)
and the [ATSC audio investigation](ATSC3-AUDIO-STARTUP-RCA-AND-FIX.md), which
this record extends rather than replaces.

## The three reports, and what was actually true

Paul reported on 2026-09-24 that Live TV "seems to transcode no matter what",
that audio is "unnecessarily downmixed to stereo — the 12-channel AC-4 is the
only thing that should need downmixing", and that every channel plays in the
same aspect ratio. The lineup on the FLEX 4K at `192.168.5.191` is 59 ATSC 1.0
channels (MPEG-2, AC-3, mostly 480i and 1080i) and 10 ATSC 3.0 channels (HEVC
Main 10 1080p60, AC-4 — and, measured on 157.1, an **AC-3 5.1 simulcast in the
same programme**, plus a Spanish and a described-video mono track).

| Report | Cause, with the anchor | What changed |
|---|---|---|
| Transcodes no matter what | Every client sent `interlaced: false` on every video limit ([web](../../crates/plurxd/src/web/pages/live-tv.js), [Apple](../../clients/apple/Sources/LiveTv.swift), [Android](../../clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvApi.kt)) and Android never claimed `mpeg2video`, so `unsupported_interlacing` or the missing codec claim sent every ATSC 1.0 channel through a video encode. On ATSC 3.0 the picture was already copied when the client claimed HEVC Main 10; only the audio was converted, and it was converted from the fragile AC-4 track. | Android TV claims a hardware MPEG-2 decoder and `interlaced: true` (television UI mode only); the owner selects the audio track a player can copy; Android claims HEVC in MPEG-TS as well as fMP4 so copied AC-3 need not be converted for the init-file race. Apple and the web cannot decode MPEG-2 in HLS, so the ATSC 1.0 picture is still encoded for them — that is the one "absolutely necessary" case. |
| Downmixed to stereo | All three clients claimed `aac: max_channels 2`, so every encode was `-ac 2`; and an AC-4 track whose layout the probe had not seen was folded to stereo before a frame existed (`source_channels.unwrap_or(2)`). | The AAC claim is the sink's real channel count (Apple `maximumOutputNumberOfChannels`, Media3 `AudioCapabilities.maxChannelCount`, web `AudioContext.destination.maxChannelCount`), floored at stereo and capped at 5.1. The encode negotiates its layout with `aformat=channel_layouts=` under that ceiling instead of `-ac N`: 7.1.4 → 5.1, 5.1 → 5.1, stereo → stereo, unknown → whatever the first frame says. `-ac` no longer appears on a live command. |
| Same aspect ratio for every source | **Not reproduced on the server.** Real captures of 6.2 (704×480, SAR 40:33, DAR 16:9, 480i) were run through the exact producer chains on nynuc — `h264_qsv` with `bwdif,format=nv12,hwupload`, `h264_vaapi`, and `libx264` — and every published segment carries SAR 40:33 / DAR 16:9. `scripts/live-tv-hardware` now records the segment's SAR/DAR so a run against the daemon proves it end to end. | Open: the squish is downstream of the segment — a client display path or the television — and needs the client named. See "Still open". |

## Audio track selection — direct play before conversion

The source probe now lists every audio stream
(`LiveSourceFacts.audio_tracks`: ordinal, codec, sample rate, channels, layout,
language). `resolve_live_delivery` chooses one track per delivery
(`LiveDeliveryPlan.audio_track`, mapped as `0:a:<n>`), in this order:

1. **A track the player copies** — codec in its capability document, a channel
   limit that admits it, and an HLS packaging that carries the (video, audio)
   pair on the video route already decided.
2. **A robust decode over a fragile one** — AC-4 waits for a global
   random-access frame the broadcast may withhold for seconds; AC-3 beside it
   decodes at once.
3. **The fuller layout**, then broadcast order.

Described-video, hearing-impaired and commentary tracks (the container's
disposition flags) rank below every main track whatever their codec. Only
tracks in the programme's primary language are candidates — the first track's
tag, or, when the first track carries none (`und` counts as none), only the
untagged tracks — so the choice never changes what language is heard. The
selected track is mapped by its PID (`0:i:0x33`) when the probe saw one, so a
stream the runtime demuxer has not yet classified cannot shift the ordinal onto
another track. On 157.1
this turns "decode AC-4, encode AAC stereo" into "copy AC-3 5.1" for a player
that takes AC-3 in MPEG-TS (Android), and "copy AC-3 → AAC 5.1" for one that
takes HEVC only in fMP4 (Apple), where the init-file race rule from the ATSC
investigation still applies.

**AC-3 in fMP4 switches container before it converts.** The rule that
converts copied AC-3 to AAC in live fMP4 now first asks whether the player
claimed the same pair in MPEG-TS; if it did, the packaging switches
(`container_switched`) and the audio stays untouched. MPEG-TS has no init file
to race. For the same reason AC-3 only counts as *copyable* in track selection
when MPEG-TS carries it: on an fMP4-only player a stereo AAC track beside a
5.1 AC-3 track is the true copy and wins.

## Channel ceiling semantics

`output.audio_channels` on an encoded route is now an **upper bound**: the
player's AAC limit, 5.1, and the source layout when the probe learned it. When
the probe did not learn it, the ceiling stands and the first decoded frame
decides. The web panel says "up to N channels" in that case; the copy route
still reports the source's count exactly.

## Android: MPEG-2 and interlacing

The AAC channel claim on a television is what the HDMI/ARC/eARC output device
advertises for PCM (`AudioDeviceInfo.channelCounts`), not Media3's
`AudioCapabilities.maxChannelCount` — that is a fixed 8 without an HDMI plug
intent and folds in bitstream passthrough masks on API 33+, so it would have
claimed 5.1 on earbuds. A handset claims stereo.

`video/mpeg2` maps to `mpeg2video` in the capability probe, and
`videoCodecCaps` admits it **only from a hardware component** — Android's
`c2.android.mpeg2.decoder` neither deinterlaces nor keeps up with 1080i. The
live envelope sets `interlaced: true` on every video limit when the UI mode is
television, because TV SoCs deinterlace in the pipeline behind MediaCodec (that
is how every Android TV live-TV app direct-plays 480i/1080i MPEG-2); a phone
or tablet weaves fields and shows combing, so it keeps `false`. The
`compatibility.failed_video` retry that already exists is the fallback if a
particular box's decoder disappoints: the next start asks for the encode.

Because the claim lives in the shared capability document, `/decision` will
also direct-play MPEG-2 **files** (DVR recordings, `.mpg`) on such a device.
That is the intended default.

## Verification

- Rust: `cargo test -p plurxd --bin plurxd -- atsc_ live_tv_delivery:: live_tv_`
  — 53 passed on nuc3 (1.97.1); `cargo clippy -p plurxd --all-targets -- -D warnings`
  clean; `cargo fmt --check` clean.
- Apple: `make apple-test` on `mba` — 1330 cases passed, including
  `testLiveEnvelopeClaimsTheRouteChannelsForAac`.
- Android: `:app:testDebugUnitTest` in the pinned image on nuc3 — 745 tests,
  0 failures, including `liveEnvelopeCarriesTheSinkFactsAndBothHevcContainers`,
  `mpeg2IsClaimedOnlyFromAHardwareDecoder` and
  `hdmiPcmChannelsTakeTheWidestSinkAndFallBackToStereo`.
- Web: `node tests/web/live-tv.test.js` (incl. the AAC ceiling case),
  `asset-load`, `page-read-budget`.
- Hardware, this branch's `plurxd` on nuc3 against the real tuner
  (`scripts/live-tv-hardware --self-host --device 192.168.5.191 --server-bin
  target/debug/plurxd`):
  - **6.2**, bodiless envelope → encode/encode; the published segment is
    `h264 704×480 SAR 40:33 DAR 16:9`, AAC stereo (the broadcast is stereo);
    first playable segment 3.9 s after asking.
  - **157.1 `--copy`** → **copy/copy**, `audio_track: 1`, MPEG-TS
    (`container_switched`); segment `hevc 1920×1080` + `ac3 5.1(side)`; first
    playable segment 6.2 s. No FFmpeg decoder ran.
- Adversarial review (one pass, eight findings): the Android channel probe,
  described-track demotion, the stricter language guard, AC-3 copyability on
  fMP4-only players, PID mapping and the web ceiling wording above are its
  outcome; the `(mpegts, hevc)` output combination it flagged is exercised by
  the 157.1 hardware run (the live playlist is a media playlist, no CODECS
  attribute to get wrong), and the Apple package has no macOS destination.

## Still open

- **The aspect-ratio report.** The server output is geometrically correct on
  every encoder family; the client showing the squish has to be named and its
  playback-info panel read ("Playing resolution" vs "Stream format"). If it is
  the Apple TV, the next check is `AVPlayerItem.presentationSize` against the
  stream's SAR on a real 480i channel; if Android, `VideoSize.pixelWidthHeightRatio`
  as PlayerView receives it.
- **Apple and the web still encode ATSC 1.0 video.** Neither can decode MPEG-2
  inside HLS. Audio on those routes is copied AC-3 (Apple) or AAC at the
  sink's channel count (web).
- Physical acceptance of MPEG-2 direct play on each Android TV box in the
  fleet (one 480i and one 1080i channel each): a GPT prompt is in the handoff.
