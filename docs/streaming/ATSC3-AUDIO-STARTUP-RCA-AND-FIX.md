# ATSC 3.0 audio — safe live startup and what the captures prove

**Status:** implemented and locally validated; not deployed ·
**Written:** 2026-09-19

Companion to [Live TV features](../FEATURES.md#4a-live-tv--the-antenna-on-every-screen)
and the [original-quality delivery record](../features/LIVE-TV-ORIGINAL-QUALITY-IMPLEMENTATION.md).
This investigation covers the September 16 macOS failures on channels 133.1,
128.1 and 103.1. The VideoToolbox caption fix remains intact.

## Three channels, three distinct failures

| Channel | Evidence | Consequence |
|---|---|---|
| 133.1 KVCW | Supplied Jellyfin FFmpeg logs decode HEVC and AC-4 without errors, but AAC rejects the 12-channel `7.1.4` layout. Converting to six or two channels succeeds. | Bound encoded AAC to at most six channels and the client's smaller limit. |
| 128.1 DEFY | The 512 KiB / 1 s input probe reports zero AC-3 channels and sample rate. Copy fails writing the HLS header. A larger probe identifies 48 kHz stereo, but short fMP4 segments can still produce an unreadable init file. | Require complete audio facts for copy; convert AC-3 to AAC in live fMP4 while preserving compatible HEVC. |
| 103.1 KSNV | Both probe budgets fail on the finite capture. Exact-version local replay consumes 175 English audio packets without producing an audio frame. All 175 lack the global AC-4 random-access flag; the 171 Spanish packets also lack it. | This capture cannot initialize a fresh decoder. It does not establish that the live channel or its audio coding is unsupported. |

The original-quality change `224fd2521` exposed the first two paths: AAC
channels were no longer fixed to stereo, and supported audio could be copied.
The earlier `90319fcf9` change shortened the input probe. Their interaction
explains why the previous re-encoding route could work with the short probe.

## An FFmpeg success exit is not a playback check

The supplied 128.1 clip delays AC-3 relative to video by about 1.99 seconds.
With the larger probe, both Homebrew FFmpeg 9.0.1 and the exact Jellyfin
8.1.2-5 build finish copying it to one-second fMP4 HLS with exit zero. They
also log `Cannot write moov atom before AC3 packets`. Reading that output
back fails with `invalid size 0 in stsd` and exit 183.

Passing `movflags=+delay_moov` through `-hls_segment_options`, or setting
`-max_interleave_delta 0`, did not repair this reproduction. Three-second
segments happened to work on this clip because audio arrived before the
first boundary; that is not a general bound on broadcast audio arrival.
Converting only AC-3 to AAC gives the muxer usable initialization metadata
and preserves the original compressed video.

## 103.1 runs out of capture before the decoder can start

The supplied `prefix-103.1.ts` is 4 MiB and about 5.86 seconds long. An
8 MiB / 10 s probe can consume all of it but cannot observe later broadcast
frames. Local replay with Jellyfin 8.1.2-5 reports zero decoded audio frames
and zero decode errors for either language, followed by audio filter
initialization failure at EOF. Changing the requested presentation also
does not produce frames.

The exact release's
[AC-4 decoder patch](https://github.com/jellyfin/jellyfin-ffmpeg/blob/v8.1.2-5/debian/patches/0054-add-ac4-decoder-for-atsc-3-0.patch)
sets `have_iframe` only after `iframe_global` is observed. Before that,
`ac4_decode_frame` consumes packets without returning frames or setting the
sample rate and channel layout. Parsing the packet headers in this capture
confirms zero global random-access flags on both audio tracks. The missing
layout at EOF follows from that wait; it is not evidence of an unsupported
layout or a new FFmpeg-version regression.

The live source probe is allowed to return unknown audio parameters. The
producer must continue decoding the original stream and can discover them
later. A zero count must never become `-ac 0`, and unknown facts must not
authorize audio copy. Actual 103.1 playback after a random-access frame
arrives remains unverified. A longer uninterrupted capture spanning such a
frame, or a real tuner run, is needed to close that question.

The historical `capability_expired / the live-TV session stopped` messages
are not proof of this decoder failure. That generic cause also covers a
cancelled session. This change preserves cancellation semantics.

## Implementation boundaries

The [delivery planner](../../crates/plurxd/src/live_tv_delivery.rs) makes
these decisions before FFmpeg starts:

1. **Treat zero audio facts as unknown.** The
   [source parser](../../crates/plurxd/src/live_tv.rs) normalizes zero channels
   and sample rate to absent values. The planner also handles zero channels
   defensively for facts arriving from other callers.
2. **Bound AAC to 5.1.** The output count is the minimum of six, the positive
   source count (stereo when unknown), and the client's AAC channel limit.
3. **Convert AC-3 for live fMP4.** The client must claim an AAC packaging
   route and the owner must have an audio encoder. The reason is
   `audio_muxer_incompatible`; video is copied when its existing claim allows
   it. AC-3 in MPEG-TS remains eligible for copy.
4. **Give audio copy a 2 MiB / 2 s probe.** Complete source-probe facts do
   not configure the separate runtime demuxer. Conversion retains its shorter
   512 KiB / 1 s probe and can acquire missing audio facts while decoding.
5. **Retain sanitized failure causes.** Unsupported channel layout, missing
   input layout, and missing sample rate are recognized across stderr chunks.
   Only fixed descriptions reach the session error; raw media metadata and
   tuner URLs do not.

**Non-goals:** change DRM policy, patch or replace an installed FFmpeg, guess
missing AC-4 decoder state, synthesize silent audio, change caption handling,
or claim acceptance on a friend's live tuner from a finite file replay.

## Repeat the regression checks

The [audio tests](../../crates/plurxd/src/live_tv/atsc_audio_tests.rs) cover
channel bounds, incomplete facts, route refusal, copy probe settings and
sanitized diagnostics. A generated HEVC/AC-3 source delays audio by two
seconds; the production command must publish while stdin is still open, and
its resulting HLS must decode both tracks without errors. A finite replay
manifest is added only after proving live publication.

```bash
# Use the repository pin; do not rely on Homebrew's default rustc.
rustup run 1.97.1 cargo test -p plurxd --bin plurxd atsc_
rustup run 1.97.1 cargo test -p plurxd --bin plurxd live_tv_delivery::tests

# Private broadcast captures stay outside the repository.
PLURX_ATSC3_AC3_CAPTURE=/path/to/cap-128.1.ts \
  rustup run 1.97.1 cargo test -p plurxd --bin plurxd \
  atsc_supplied_ac3_capture_produces_decodable_live_fmp4 -- --ignored
```

**How to read it:** a passing replay means the actual server command produced
decodable HEVC/AAC HLS from the supplied file, including publication before
EOF. It does not measure real-time tuner startup or prove physical client
playback. The 133.1 downmix evidence comes from the supplied FFmpeg logs;
that channel's media capture was not supplied.

Capture identities (SHA-256):

| File | SHA-256 |
|---|---|
| `cap-128.1.ts` | `4c04c11bf47cfe0c11ab9111c4ff6fb0c62d234a7697395bbba3f82aa083ba31` |
| `prefix-103.1.ts` | `d7cebb3f188098e533bf6a2718cd8527ddbef3b55876b4e362934ef662550b6f` |

The portable Jellyfin build was downloaded from its official release and
verified against the release asset's SHA-256:
`b2ac80bb184e9a2f3f7c236876b2f56a5639596a95b42f41ced34fab66ad720d`.
It was run from a temporary directory; the installed FFmpeg was unchanged.

Local validation on Rust 1.97.1:

- The Live TV test selection passed: 157 tests, with the two opt-in hardware
  and private-capture tests excluded from the default selection. Tests with
  fake tuner listeners required local socket permission outside the sandbox.
- The private 128.1 capture test passed separately with Jellyfin 8.1.2-5,
  using `PLURX_FFMPEG` and `PLURX_FFPROBE` to select the portable binaries.
- `cargo clippy -p plurxd --all-targets -- -D warnings`, Rust formatting and
  the four documentation-index checks passed.
