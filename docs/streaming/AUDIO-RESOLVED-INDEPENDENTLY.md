# Audio resolved independently — the picture's rung stops deciding the sound

**Status:** ready for review · **Executes:** Q5 / §3.1.2 / F-stream-5 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Read the review's §3.1.2, the assessment's Q5 and F-stream-5 rows
([ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20-ASSESSMENT.md)),
and "Audio owns a global sample lattice" in
[VOD-ENCODING.md](VOD-ENCODING.md) — the encoded-VOD audio contract is
AAC-specific in a way that bounds what this plan may change there. Work
the milestones in order; one draft PR each into `main` under the fast lane.
Every `file:line` was read at `88a3957a` and is marked **re-verify at build
time**. If a step seems to require changing the VOD audio lattice
(`aresample=48000`, AAC 1024-sample frames, film-global phase), the copy
path's `-channel_layout:a 5.1` for six channels, or the meaning of the
`aaction` recipe field for existing keys, stop and flag it.

**Correction to the review's source material (the review already carries
it):** F-stream-5's "no sample-rate control" is false for encoded VOD
(`-ar 48000`, [`vod.rs:266-267`](../../crates/plurx-core/src/transcode/vod.rs));
it is true for the rolling path only. One further constraint the review
does not state: an E-AC-3 frame is 1,536 samples and a 2.002 s VOD GOP is
96,096 samples = 62.5 E-AC-3 frames, so E-AC-3 cannot share the AAC lattice
(96,096 = 93.84 × 1,024 either — the lattice is film-global, not per-GOP,
which is why VOD-ENCODING.md anchors each generation to a global AAC frame
boundary). E-AC-3 on encoded VOD is therefore a lattice design of its own
and is **out of this plan**; VOD gets 5.1 AAC, which keeps the lattice.

## 1. Objective

1. Negotiate audio from three facts — the selected source stream, the
   client's *sink* (channels it can actually reproduce, codecs it can
   decode or pass through), and the route — and carry the result into
   `TranscodeOptions`, the recipe identity, the argv, the playlist
   `CODECS`, cache identity, offline packaging and prepared handoffs.
2. A full video transcode (tone-map, subtitle burn, lower rung) no longer
   downmixes compatible audio to stereo AAC 160 k: copy it when the route
   allows, else encode to `eac3`/`ac3` when the sink claims it, else AAC at
   the sink's channel count.
3. Put `-ar 48000` on the rolling path, so lossy output never inherits a
   96 kHz source rate.
4. Downmix to stereo only when the sink is stereo, with an explicit `pan`
   matrix chosen per source layout and verified for dialogue level and
   clipping — not swresample's normalised default.

## 2. Contract today

```text
 profile.audio_codecs (names)      source.audio_streams[i].{codec,channels}
              \                          /
               v                        v
    Decision.transcode_audio: bool  (playback/mod.rs:674)
                     |
        +------------+-------------+
        v                          v
  copy-video path             full transcode path
  copy_input_args             options_for_tone_map -> ..Default::default()
  aac 320k + 5.1 for 6ch      audio_channels: 2, audio_bitrate_kbps: 160
  aac 256k otherwise          hls_args_inner: -c:a aac -ac 2 -b:a 160k
  (no -ac for 7.1/8ch)        (no -ar)
                     |
                     v
  Recipe::hash "aaction" = "copy" | "aac"   (recipe.rs:121-125)
  plan_digest: audio_channels, audio_bitrate, audio_index, audio_offset_ms
```

- [`mod.rs:895-914`](../../crates/plurx-core/src/transcode/mod.rs) —
  `TranscodeOptions::default()`: `audio_channels: 2`, `audio_bitrate_kbps:
  AUDIO_BITRATE_KBPS_DEFAULT` (160, `:893`, "public because the quality
  ladder's advertised totals must include the audio").
- [`transcode.rs:14576-14665`](../../crates/plurxd/src/transcode.rs) —
  `options_for_tone_map` sets video fields and ends `..Default::default()`
  (`:14663`); nothing sets `audio_channels`. `rg -n audio_channels
  crates/plurxd/src` outside tests: only Live TV.
- `mod.rs:1655-1666` — `hls_args_inner`: `-c:a aac -ac {audio_channels}
  -b:a {audio_bitrate_kbps}k`, unconditional AAC, no `-ar`.
- `mod.rs:2028-2059` — copy-video audio conversion: `-c:a aac`, `320k` +
  `-channel_layout:a 5.1` when `copy_audio_channels() == Some(6)`, else
  `256k` with the source channel count and no `-ac` (a 7.1 source becomes
  7.1 AAC-LC, no sample-rate change). The comment records why the 5.1
  layout is forced (AVPlayer −12848 on a PCE-signalled `5.1(side)`).
- `vod.rs:244-269` — encoded VOD audio: `aresample=48000:async=1`, `-ar
  48000 -profile:a aac_low`; VOD-ENCODING.md §"Audio owns a global sample
  lattice".
- [`recipe.rs:121-125`](../../crates/plurx-core/src/transcode/recipe.rs) —
  `field(&mut h, "aaction", if self.audio_copied { b"copy" } else { b"aac" })`;
  [`decode.rs:2078-2097`](../../crates/plurx-core/src/transcode/decode.rs)
  feeds `audio_channels`, `audio_bitrate`, `audio_index`, `audio_offset_ms`,
  `input_has_audio`.
- [`playback/mod.rs:118-202`](../../crates/plurx-core/src/playback/mod.rs) —
  `DeviceProfile` has `audio_codecs: Vec<String>` and no channel count;
  [`caps.rs:168-217`](../../crates/plurx-core/src/playback/caps.rs) —
  `DeviceCaps.audio: Vec<String>`; the flat query's `acodec`
  ([`stream.rs:176-177`](../../crates/plurxd/src/http/stream.rs)) is a codec
  list. [`profiles.toml`](../../crates/plurx-core/src/playback/profiles.toml)
  lists `eac3`, `ac3`, `dts`, `truehd` for `directplay-any`. **A codec name
  says the decoder exists; it says nothing about how many channels the
  route can reproduce** — an Apple TV on a soundbar over ARC, an iPhone on
  AirPods and a Shield on an AVR all list `eac3`.
- `playback/mod.rs:668-674` — `Decision.transcode_audio: bool`; the forced
  transcode arm (`:1532-1535`) sets it `true` unconditionally.
- Master playlist `CODECS` for transcodes: `transcoded_hls_codecs`
  ([`transcode.rs:26709-26717`](../../crates/plurxd/src/transcode.rs))
  appends `mp4a.40.2` to every grade; no `ec-3` spelling exists on that
  path (a copy fixture at `:34918` shows `ec-3` is already handled for
  copied E-AC-3).
- Offline packages persist the effective rate control at creation; audio
  is whatever the recipe produced. Prepared replacements create a new
  session through the same create path.

## 3. Change

### 3.1 The sink claim (client → server)

`DeviceCaps` gains `audio_sinks: Vec<AudioSink { codec: String,
max_channels: u8, passthrough: bool }>` (v2 JSON) and the flat query gains
`achannels=<n>` (a single ceiling, the legacy shape's best effort). Absent
means **2** — exactly today's behaviour for every client until it learns to
report. Sources of truth per client, each to be verified on a device before
the client sends it (Q11's rule: a fixture string is not decoder evidence):

- Apple: `AVAudioSession.sharedInstance().currentRoute.outputs` channel
  counts and `maximumOutputNumberOfChannels`; E-AC-3 passthrough is claimed
  only when the route is HDMI and the receiver's channel count > 2.
- Android: Media3 `AudioCapabilities.getCapabilities(context)` —
  `supportsEncoding(ENCODING_E_AC3)` and `getMaxChannelCount()`; the
  route-aware probe D3 mentions already exists.
- Web: `AudioContext.destination.maxChannelCount`; browsers without E-AC-3
  claim none (Safari on macOS may; test).

`DeviceProfile` gains `max_audio_channels: HashMap<String, u8>` keyed by
codec, translated from either wire shape by the one function both shapes
already share (caps.rs's `LegacyCaps` route).

### 3.2 Negotiation (server, pure function, fixtures shared with clients)

`resolve_audio(source_stream, profile, route) -> AudioDelivery` in
`plurx_core::playback`:

```text
AudioDelivery { action: Copy | Encode { codec, channels, layout, bitrate_kbps, sample_rate: 48000 },
                downmix: Option<DownmixMatrix>, reason: &'static str }
```

Order of preference, first that fits:

1. **Copy** — codec in `profile.audio_codecs`, channels ≤
   `max_audio_channels[codec]`, container/transport admits it (HLS TS:
   AAC/AC-3/E-AC-3/MP3; fMP4: those plus FLAC/ALAC where claimed), no A/V
   offset correction (a copied stream cannot take `-af`), sample rate
   admitted by the sink.
2. **Encode E-AC-3** at `min(source, sink)` channels, 640 k for 6 ch (Apple
   HLS authoring: 5.1 E-AC-3 ≤ 640 kb/s), 384 k for 2 ch — when the sink
   claims `eac3` with ≥ 6 channels. **AC-3** likewise at 640 k when only
   `ac3` is claimed.
3. **Encode AAC** at the sink's channel count: 6 ch → `-ac 6
   -channel_layout:a 5.1 -b:a 320k` (the copy path's proven pairing), 2 ch →
   `-b:a 160k` (today's value) with the M4 downmix matrix.
4. Downmix only when `max_channels < source channels`, or the viewer asked
   (a future setting; not in this plan).

The route argument matters: encoded VOD returns `Encode { codec: aac, … }`
only — §"Correction" — while the rolling path and the copy-video path may
return E-AC-3/AC-3. Audio offset correction still forces `Encode`.

### 3.3 Carrying the decision

- `TranscodeOptions` gains `audio: AudioDelivery` (replacing the three loose
  fields over two PRs: add, then remove). `hls_args_inner` emits `-c:a
  {codec} -ac {channels} [-channel_layout:a {layout}] -b:a {kbps}k -ar 48000
  [-af pan=…]`; the copy-video path's conversion branch is generated from
  the same `AudioDelivery` so the two paths cannot drift.
- `plan_digest`: `audio_codec`, `audio_layout`, `audio_sample_rate`,
  `audio_downmix` (matrix id) added beside the existing audio fields.
  `Recipe::hash`'s `aaction` keeps emitting `copy`/`aac` for AAC and copy
  so **no existing key moves**; it emits `eac3`/`ac3` for the new codecs.
  `CACHE_RECIPE_VERSION` stays 3. Only sessions whose negotiation now
  differs from "stereo AAC 160 k" get new keys — which is the point.
- `Decision` gains `audio: AudioDelivery` beside `transcode_audio` (kept in
  step for old clients: `transcode_audio = matches!(audio.action,
  Encode{..})`).
- `Rung.total_kbps` / `peak_kbps` ([`transcode.rs:26794-26808`](../../crates/plurxd/src/transcode.rs))
  add the negotiated audio bitrate, not `AUDIO_BITRATE_KBPS_DEFAULT`, so
  [HONEST-MASTER-PLAYLIST.md](HONEST-MASTER-PLAYLIST.md)'s BANDWIDTH stays
  true; `AUDIO_BITRATE_KBPS_DEFAULT` remains the value for an absent claim.
- `HlsContext.codecs`: `mp4a.40.2` | `ec-3` | `ac-3` from the delivery.
  plurx muxes audio into the variant (no `EXT-X-MEDIA` audio group), so
  `CHANNELS` — an `EXT-X-MEDIA` attribute — does not apply; if an audio
  group is ever introduced, `CHANNELS="6"` comes from the same struct.
- Playback info / badges: `delivered_audio` (`codec`, `channels`, `action`)
  on `/decision` and the session response, the way `delivered_dynamic_range`
  is carried.
- Prepared handoffs: the successor inherits the incumbent's `AudioDelivery`
  unless the request changed the audio index or the sink claim; a successor
  with different audio is refused as a prepared replacement and offered as
  a reopen instead (an in-place switch that changes channel count is a
  receiver re-lock and an audible gap).
- Offline packaging: `AudioDelivery` persisted with the package like the
  rate-control snapshot (one column, `audio_recipe TEXT`, SQLite `v64` /
  replicated `v43` or the next free numbers — coordinate with the other
  September migrations).

### 3.4 The stereo downmix matrix (M4)

swresample's default (`center_mix_level` 0.707, `surround_mix_level`
0.707, LFE dropped, coefficients normalised so their sum ≤ 1) lowers a 5.1
mix's dialogue by roughly 7–8 dB relative to the centre channel's own
level — the "quiet dialogue on transcodes" report. The plan replaces it
with an explicit `pan` per **source layout**, using channel *names* so the
matrix does not depend on channel order (the assessment's objection to the
appendix's numeric string):

```text
5.1 / 5.1(side):  pan=stereo|FL=FL+0.707*FC+0.707*{BL|SL}|FR=FR+0.707*FC+0.707*{BR|SR}
7.1:              pan=stereo|FL=FL+0.707*FC+0.5*SL+0.5*BL|FR=FR+0.707*FC+0.5*SR+0.5*BR
6.1, 4.0, 3.0, 2.1: their own rows, same rule (centre −3 dB, surrounds
                  −3 dB or −6 dB when two pairs fold, LFE omitted)
```

These are the ITU-R BS.775 Lo/Ro shape and are **starting values**: M4's
acceptance chooses the final gains by measurement (dialogue level within
±1 LU of the source centre channel's contribution; sample peaks ≤ −1 dBFS on
the loudest scene of each fixture; `alimiter` added only if a fixture
clips at the chosen gains). The `pan` `=` form keeps gains as written (the
`<` form renormalises, which is the behaviour being replaced). Whichever
gains are chosen become the `DownmixMatrix` id in the digest.

## 4. Guardrails (non-goals)

- **Encoded VOD stays AAC** (48 kHz lattice); 5.1 AAC is admitted because
  it keeps the frame size; E-AC-3 on VOD is a separate design.
- **The copy path's `-channel_layout:a 5.1` for six channels stays**; the
  new builder reproduces it byte-for-byte for the AAC-6ch case (test pins
  the argv).
- **Existing recipe keys for stereo-AAC and copy do not move**: `aaction`
  spellings preserved.
- **A codec name is not a sink.** Absent `audio_sinks` → 2 channels; no
  client is upgraded to 5.1 by the server's guess.
- **Prove each container/transport/codec/channel tuple on a device before
  a client claims it** (Q11). The server plan lands first with no client
  claiming more than stereo; each client's claim is its own PR with device
  evidence.
- **The pan string is illustrative until M4 measures it** (assessment
  F-stream-5); no "fixed improvement" is promised.
- **No loudness normalisation** (`loudnorm`) in the live path.
- **Audio-offset correction still forces an encode** — a copied stream
  cannot be filtered.
- **Bluetooth / stereo fallback**: when the route changes mid-session the
  client re-reports its sink on the next create; the server does not guess
  from the User-Agent.

## 5. Milestones

### 5.1 M1 — `resolve_audio` and the sink claim (no output change)

Pure function with fixtures under `tests/playback/` shared by the three
clients (as the decision fixtures are); `DeviceCaps.audio_sinks`,
`achannels`, `DeviceProfile.max_audio_channels`; `Decision.audio`;
`delivered_audio` in responses. With no client sending a claim, every
answer equals today's behaviour (stereo AAC 160 k on transcodes; the copy
path's rule on copies).

Tests: `cargo test -p plurx-core playback::audio` — 5.1 TrueHD source with
`eac3@6` claimed on the rolling route → `Encode{eac3,6,640}`; same with VOD
route → `Encode{aac,6,320,layout 5.1}`; AAC 5.1 source with `aac@6` → Copy;
same with offset ≠ 0 → Encode; stereo claim → stereo AAC with a 5.1 matrix
id; absent claim → today's answer. `node tests/playback/*.test.js` fixtures
for the three clients' translation of the claim.

Acceptance: `cargo test -p plurx-core playback` green; `curl -s -X POST
:32400/api/v1/files/<id>/decision -d @caps-v2.json | jq .audio` shows the
struct, and with `caps-v2.json` lacking `audio_sinks` shows `channels: 2`.

### 5.2 M2 — carry it through options, argv, digest, manifests

`TranscodeOptions.audio`; `hls_args_inner` and the copy conversion branch
generated from it; `-ar 48000` on the rolling path; `plan_digest` fields;
`aaction` spellings; `HlsContext.codecs` audio component; `Rung` totals
from the negotiated bitrate; offline `audio_recipe` column; prepared
handoff inheritance rule.

Tests: argv baselines (`cargo test -p plurx-core transcode`) — the stereo
AAC line gains exactly `-ar 48000` and nothing else; the 6-ch AAC line
equals the copy path's `320k -channel_layout:a 5.1`; E-AC-3 line `-c:a
eac3 -ac 6 -b:a 640k -ar 48000`; `cargo test -p plurx-core recipe` — a
stereo-AAC recipe hash is unchanged from `88a3957a` (extend the golden
table), an E-AC-3 recipe differs; `cargo test -p plurxd hls` master codecs
`avc1…,ec-3`; `cargo test -p plurxd offline` snapshot round-trip; `cargo
test -p plurxd prepared` refusal when audio differs.

Acceptance: golden hash unchanged; new hashes differ; `make unit` green.

### 5.3 M3 — first client claim, on a device

Apple first (the AVR case the review names). The client reports
`audio_sinks` from the current route; a tvOS build on the living-room Apple
TV over HDMI to the receiver.

GPT prompt:

```text
Living-room Apple TV (HDMI → AVR), plurx build with the audio claim:
1. Play Night Tide (HEVC, TrueHD 7.1) with subtitles burned so the video
   transcodes. Record: the receiver's front-panel format (should read
   Dolby Digital+ 5.1, not PCM 2.0), `delivered_audio` in the session
   response, the session's ffmpeg audio arguments in journalctl, and
   A/V sync by ear at two points.
2. Change quality to 720p mid-play (prepared handoff). Record: whether the
   receiver re-locks, any gap, and that `delivered_audio` is unchanged.
3. Switch the Apple TV's audio output to AirPods, reopen playback. Record:
   `delivered_audio.channels == 2`, the receiver silent, dialogue level
   compared with the stereo mix by ear against a direct-play stereo title.
4. Repeat 1 with an AAC 5.1 source (Harbor Lights) — expect
   `action: copy`, receiver reads Multichannel PCM or Dolby depending on
   the box's own output setting; note which.
```

Acceptance: the four observations recorded in the PR; the receiver reads a
multichannel format in step 1; no second live session in Activity after
step 2.

### 5.4 M4 — the downmix matrix, measured

Fixtures: three 5.1 clips (dialogue-centred drama scene, action scene with
LFE, music), one 7.1, generated or Paul-named (Harbor Lights, Night Tide),
plus a synthetic clip with −20 dBFS pink noise on FC only and one with
−3 dBFS on all channels (clipping worst case).

Measurements per fixture, `88a3957a` default downmix versus the M4 matrix:
`ebur128` integrated loudness and the FC-only clip's output level
(dialogue); `astats` sample peak and count of clipped samples on the
all-channels clip; listening notes from one device.

Acceptance: FC-only clip lands within ±1 LU of −20 LUFS − 3 dB per side
(the −3 dB centre split); zero clipped samples on the worst case or an
`alimiter` stage with its own before/after; `pan` strings per layout
committed with the numbers in the PR; `cargo test -p plurx-core
transcode::audio` pins each layout's string.

### 5.5 M5 — Android and web claims

Each its own PR with the device evidence Q11 requires (Shield on the same
AVR; a phone on Bluetooth; Safari/Chrome on the laptop). The server needs
no change.

Acceptance: per client, the M3 observations repeated; a phone claim of 2
channels produces the M4 stereo matrix path.

## 6. Verification and rollout

- Fast lane `make unit`; focused: `cargo test -p plurx-core playback`,
  `cargo test -p plurx-core transcode`, `cargo test -p plurx-core recipe`,
  `cargo test -p plurxd hls`, `cargo test -p plurxd offline`; Node gate
  `node tests/playback/web-policy.test.js` (currently red at `:6007` for an
  unrelated stale assertion per review §4.8 — fix that first or the new
  fixtures cannot be called green).
- Metrics: `plurx_audio_delivery_total{action="copy"|"encode",
  codec="aac"|"eac3"|"ac3", channels="2"|"6"|"8"}` — bounded labels; alert
  owner Paul; expected demand: every HLS session; window one week after
  each client claim lands. A fleet where `encode/aac/2` stays at 100 % after
  M3 says the claim is not reaching the server.
- Settings: none. The sink claim is client evidence; a viewer preference for
  stereo is §7.
- Rollout: M1 → M2 (server, no visible change) → M3 (Apple) → M4 → M5.
  Schema change in M2 deploys as every schema change does.
- Rollback: a client can stop sending the claim; the server answers as
  today.

## 7. Open questions

1. A viewer preference ("always stereo", "prefer surround") — where it
   lives (per user, replicated) and whether it belongs in the create request
   like `audio=` does. Not in this plan.
2. E-AC-3 on encoded VOD: the lattice design (1,536-sample frames against
   the 48 kHz film-global grid) is its own document once M3 shows E-AC-3
   is wanted on VOD routes.
3. Whether AAC 5.1 in MPEG-TS on the rolling path is accepted by every
   client that will receive it (the copy path proves it in fMP4 for
   AVPlayer; TS is untested here) — part of M3's device evidence.
4. Whether `FRAME-RATE`-style honesty should extend to a future audio
   `EXT-X-MEDIA` group so multiple layouts can be offered at once; out of
   scope.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| | | | | | |
