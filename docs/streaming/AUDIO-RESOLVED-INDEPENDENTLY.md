# Audio resolved independently — the picture's rung stops deciding the sound

**Status:** open — M1–M5 on `main` since 2026-10-04 (#793); the Lo/Ro fold
and −4 dBFS limiter apply to typed stereo encodes (not progressive untyped,
legacy untyped or Live TV); synthetic subset measured 2026-09-30; listening
notes and device routes owed; Paul ratified on 2026-10-05 the fold and
limiter (pending one listening pass) and Decision 6 (a decoding client's route
does not refuse a copy) (see the 2026-10-04 relevance pass §2.15) · **Executes:** Q5 / §3.1.2 / F-stream-5 from
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
time**. All milestones remain logical commits in one draft plan PR, per the
work-board protocol. If a step seems to require changing the VOD audio lattice
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
max_channels: u8, passthrough: bool, sample_rates_hz: Vec<u32> }>` (v2 JSON) and the flat query gains
`achannels=<n>` (a single ceiling, the legacy shape's best effort). Absent
keeps the legacy codec-only copy rule and the existing stereo AAC full-
transcode default. Treating absence as a new two-channel refusal would remux
and downmix existing AAC 5.1 direct plays before any client learned to report
its route, contradicting M1's no-output-change requirement. Sources of truth
per client, each to be verified on a device before the client sends it (Q11's
rule: a fixture string is not decoder evidence):

- Apple: `AVAudioSession.sharedInstance().currentRoute.outputs` channel
  counts and `maximumOutputNumberOfChannels`; E-AC-3 passthrough is claimed
  only when the route is HDMI and the receiver's channel count > 2.
- Android: Media3 `AudioCapabilities.getCapabilities(context)` —
  `supportsEncoding(ENCODING_E_AC3)` and `getMaxChannelCount()`; the
  route-aware probe D3 mentions already exists.
- Web: `AudioContext.destination.maxChannelCount`; browsers without E-AC-3
  claim none (Safari on macOS may; test).

`DeviceProfile` keeps decoder claims, route sinks and passthrough claims as
different facts. It gains `max_audio_channels` for the channel ceiling plus
the complete normalized sink claims and the explicitly reported decoder set;
the sink codec is never appended to the decoder list. `AudioStream` records
the positive ffprobe sample rate. A sink's absent rate list and an older
catalog row's absent source rate are both no claim, not wildcards. The flat
`achannels` form is rejected outside 1–16 at deserialization and carries no
sample-rate claim, so it cannot newly authorize copy.

### 3.2 Negotiation (server, pure function, fixtures shared with clients)

`resolve_audio(source_stream, profile, route) -> AudioDelivery` in
`plurx_core::playback`:

```text
AudioDelivery { action: Copy | Encode { codec, channels, layout, bitrate_kbps, sample_rate: 48000 },
                downmix: Option<DownmixMatrix>, reason: &'static str }
```

Order of preference, first that fits:

1. **Copy** — an explicit decoder claim or a true passthrough claim, channels ≤
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
  `CACHE_RECIPE_VERSION` is 4 and an `arate=source|48000` field records the
  byte-changing fixed-rate decision. Existing version-3 entries miss rather
  than being served under the new output contract.
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
  is carried. `/decision` already uses `audio` for its selectable-track list,
  so the delivery field cannot use that shorter name without being silently
  overwritten during flattened response serialization.
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

**2026-09-30 measured correction:** the preceding attenuation description
was the original hypothesis, not a universal shipped-engine result.
[Jellyfin 8.1.3 synthetic measurements](AUDIO-DOWNMIX-SYNTHETIC-QUALIFICATION.md)
found the default and explicit pan both split FC by approximately −3 dB
per side, including actual AAC 160 kb/s output. No dialogue-improvement
claim follows. The named matrices clipped the coherent worst case; a
−1 dBFS pre-AAC limiter failed the decoded −1 dBFS bound, while a −2 dBFS
limiter with auto-level disabled met it on these synthetic inputs. Gains
and limiter semantics remain candidate production recipes until their
actual propagation, VOD join and content acceptance checks are proved.

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
- **The cache rotates when byte semantics change.** `aaction` spellings stay
  stable, while recipe v4 and `arate=source|48000` prevent a 48 kHz encode
  from reusing an artifact created before sample-rate pinning.
- **A codec name is not a sink.** Absent `audio_sinks` preserves the legacy
  codec-only copy rule and stereo full-transcode default; no client gains a
  new surround encode from the server's guess.
- **Prove each container/transport/codec/channel tuple on a device before
  a client claims it** (Q11). The server plan lands first with no client
  claiming more than stereo; each client claim is its own logical commit in
  this plan PR and carries its device evidence.
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
`eac3@6/48000` and decoder or passthrough evidence on the rolling route → `Encode{eac3,6,640}`; same with VOD
route → `Encode{aac,6,320,layout 5.1}`; AAC 5.1 source with `aac@6` → Copy;
same with offset ≠ 0 → Encode; stereo claim → stereo AAC with a 5.1 matrix
id; absent claim → today's answer. `node tests/playback/*.test.js` fixtures
for the three clients' translation of the claim.

Acceptance: `cargo test -p plurx-core playback` green; `curl -s -X POST
:32400/api/v1/files/<id>/decision -d @caps-v2.json | jq .delivered_audio` shows the
struct. With `caps-v2.json` lacking `audio_sinks`, a full transcode shows
stereo AAC while a compatible progressive AAC 5.1 source preserves the
legacy copy answer.

Implemented server-side in `142502010`: the v2 and flat claims share one
translation; malformed, out-of-range and duplicate claims fail closed;
negotiation is pure and route-aware; the final post-subtitle decision is
serialized as `delivered_audio`. No client emits `audio_sinks`, so this adds
readiness information without enabling surround delivery.

### 5.2 M2 — carry it through options, argv, digest, manifests

`TranscodeOptions.audio`; `hls_args_inner` and the copy conversion branch
generated from it; `-ar 48000` on the rolling path; `plan_digest` fields;
`aaction` spellings; `HlsContext.codecs` audio component; `Rung` totals
from the negotiated bitrate; offline `audio_recipe` column; prepared
handoff inheritance rule.

Tests: argv baselines (`cargo test -p plurx-core transcode`) — the stereo
AAC line gains exactly `-ar 48000` and nothing else; the 6-ch AAC line
equals the copy path's `320k -channel_layout:a 5.1`; E-AC-3 line `-c:a
eac3 -ac 6 -b:a 640k -ar 48000`; `cargo test -p plurx-core recipe` — the
version-4 stereo-AAC recipe hash pins `arate=48000`, copied audio pins
`arate=source`, and the old version-3 key is intentionally unreachable;
`cargo test -p plurxd hls` master codecs
`avc1…,ec-3`; `cargo test -p plurxd offline` snapshot round-trip; `cargo
test -p plurxd prepared` refusal when audio differs.

Acceptance: the version-4 golden is pinned; new hashes differ; `make unit` green.

The independently safe sample-rate slice began in `659fb6372`:
every lossy rolling/full-transcode and copy-conversion path now emits `-ar
48000`, including progressive remux conversion, while copied audio remains
untouched. The review disposition then added source and sink sample-rate facts,
kept decoder and passthrough trust separate, and rotated recipe identity to v4.
The source-layout continuation below retains opaque facts; it does not
interpret speaker positions. The dated synthetic receipt now measures the
three proposed layouts, including failed −1 dB decoded-AAC margin and the
passing synthetic −2 dB candidate. Real-content/phase and device evidence
remains owed; no shipped client sends a sink claim.

**2026-10-01 M2 implementation continuation:** the accompanying draft carries
the already-resolved typed audio decision through rolling/encoded-VOD/copy
options and actual argv, recipe/digest and VOD rendition identity, master
CODECS and rung budgets, durable offline snapshots, remote owner response
facts, and prepared-successor inheritance/refusal. Legacy absent claims keep
their previous argv and v4 golden identity. Encoded VOD admits only its fixed
AAC/48 kHz lattice; old offline rows receive null, not a guessed new recipe.
Copy bitrate headroom remains a conservative 640 kb/s estimate, not a probed
source rate. A canonical bounded sink claim keys request intent separately
from the server answer; explanatory text does not select argv or cache keys.
Prepared video-only switches retain actual producer audio, while incompatible
audio changes require reopen. No concrete pan, limiter, new client claim,
readiness/layout feature gate, or count-to-speaker-map inference is enabled.

The continuation in [draft #665](http://forge.lan:3000/noirr/plurx/pulls/665)
composes effort `ec79f4b34` with runtime main `5c99538fd` and the later
CI-only main `e82e36d62`, then landed queued-publication main `1b2ae4f62`:
the latter's reason-bearing yield outcomes, admission ownership, source fences
and publication regressions are retained additively. The audio option owners
and their two regressions remain unchanged. The ownership census combines
the prior413 with the new test-owned, bounded and reaped child: measured414.
Startup settings snapshots
and phase timings, worker accounting, and the landed #657 parser floor are
retained. Initial transcodes carry the canonical claim until the actual
rolling or encoded producer selects its route. A retained producer snapshot
then takes precedence over that claim and refreshed source facts; encoded
VOD refuses an incompatible retained snapshot rather than replacing it.
The actual options-owner regressions exercise rolling and VOD argv plus
plan/recipe identity with retained AAC six-channel audio and current stereo
facts. Restoring the old resolution order fails both; the fixed owners pass.
The pinned Rust 1.97.1 locked workspace/all-target check and normal hook are
recorded for each composed source in the PR. On the prior `e61537747` source,
seven focused core tests, eleven daemon tests (including both actual
owners and three parser-floor cases), two real-backend offline tests, nine
retained numeric/docs-index checks and the ownership census passed. The normal
hook passed catalog/formatting, workspace/all-target Clippy with denied warnings
and all seventy served JavaScript syntax checks. The draft records the final
committed-tree check separately. Under the one-passing-run-per-PR rule, unchanged
tests are not repeated for a base refresh; their original source receipts are
retained, not relabeled as new-tree executions. M4 content/listening and M3/M5
device-route acceptance remain open.

The subsequent current-effort refresh composes `abdc6bf62` additively, including
the delivered-codec contract, shared local-media pump, clock observer and
unit-once workflow, then `ae879d163` adds verified build packaging. Audio
retained-authority and initial-route owners remain
unchanged. The merged structural census measures task688, time1127 and
process-launch417; those are source measurements, not repeated unit runs.
Current compiler/hook receipts are recorded separately in #665; the original
nineteen regression declarations and sole review #28 disposition are retained.

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

**2026-10-02 client claim built (claude-opus-5-5):** each capability snapshot
reads `AVAudioSession` once — `maximumOutputNumberOfChannels` and whether an
output port is HDMI — and the v2 document carries `audio_sinks`: every codec
AVPlayer decodes reaches the route's channel count (stereo floor, 7.1 cap),
and AC-3/E-AC-3 are passthrough only on a multichannel HDMI route. The claim
is logged with the capability line. The four observations above remain owed
on the living-room Apple TV.

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

**2026-09-30 synthetic subset:** the linked
[numeric receipt](AUDIO-DOWNMIX-SYNTHETIC-QUALIFICATION.md) records true
`5.1`, `5.1(side)` and `7.1` inputs, separate per-side LUFS/RMS, sample
peaks/full-scale counts, default versus named matrices, and both insufficient
and sufficient synthetic AAC limiter margins. Executed collector bytes,
source/PCM hashes, actual argv and numeric JSON are retained. This is not
the three real-content scene comparisons, 7.1 content, listening notes,
device evidence, production argv pinning, or complete M4 acceptance.

**2026-10-02 real-content measurement and shipped fold (claude-opus-5-5):**
the [real-content receipt](AUDIO-DOWNMIX-REAL-CONTENT-QUALIFICATION.md)
measured nine film windows (DTS-HD MA, TrueHD 7.1, E-AC-3, AC-3, AAC). The
incumbent `-ac 2` fold already met the −3 dB centre split on most sources but
was unlimited — decoded AAC reached full scale on three windows — and applied
AC-3/E-AC-3 stored downmix levels (dialogue 1.5 dB low on both E-AC-3 titles).
The §3.4 string as written hard-clips inside `pan` on 32-bit decoders, and the
synthetic −2 dBFS ceiling failed after AAC overshoot. `DownmixMatrix` now
carries `lo_ro_5_1_back`, `lo_ro_5_1_side`, `lo_ro_7_1` and `limited_default`:
float conversion, the named matrix chosen only from the source's own layout
spelling with a matching channel count, and one −4 dBFS look-ahead limiter;
other layouts take the float default fold with the same limiter. Each fold's
exact filter chain feeds `plan_digest`, so folded keys move once and any later
gain change moves them again; unfolded and incumbent keys do not move. The
fold and the A/V correction travel as one `-af` (ffmpeg keeps only the last),
and encoded VOD runs the fold ahead of its sample-lattice trim — generation
joins are bit-identical in pre-AAC PCM. Non-stereo targets keep the incumbent
fold. Still owed: listening notes on the three heavily limited windows.

### 5.5 M5 — Android and web claims

Each client slice remains a logical commit in this plan PR, with the device
evidence Q11 requires (Shield on the same AVR; a phone on Bluetooth;
Safari/Chrome on the laptop). The server needs no change.

Acceptance: per client, the M3 observations repeated; a phone claim of 2
channels produces the M4 stereo matrix path whenever the server encodes for it
(a DTS/TrueHD source, an A/V offset, or a full transcode of undecodable audio);
decodable AAC/AC-3/E-AC-3 still direct-plays or copies under decision 6.

**2026-10-02 client claims built (claude-opus-5-5):** Android spells
`audio_sinks` from the probes it already had — the HDMI sink's PCM channel
count (`liveSinkFacts`, stereo on a handset or panel speakers) for decoded
codecs, and Media3 `AudioCapabilities` bitstreams as AC-3/E-AC-3 passthrough
at the format's channel count. The web client reads
`AudioContext.destination.maxChannelCount` once at boot (closing the probe
context) and claims that count for the codecs it decodes, never
passthrough. Under decision 6 a phone's stereo claim still direct-plays and
copies decodable 5.1; the M4 stereo fold applies whenever the server encodes
for it (DTS/TrueHD sources, offset correction, full transcodes of
undecodable audio). Device observations remain owed.

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

## 8. Implementation decisions

1. **The response field is `delivered_audio`.** `DecisionResponse.audio`
   already owns the selectable track list; Serde flattening otherwise lets the
   later list overwrite the negotiated object. A focused serialization test
   pins the distinct top-level field.
2. **An absent sink claim preserves legacy copy behavior.** The plan called
   absence "2 channels" and also required no output change. Those statements
   conflict for existing compatible AAC 5.1 direct plays, so compatibility
   wins until a client supplies measured route evidence.
3. **Unmeasured downmixes carry a requirement, not invented gains.** The pure
   decision reports `requires_layout_measurement` with the source channel
   count. *(2026-10-02: stereo folds are now measured — see §5.4; only
   non-stereo targets keep this requirement.)* M2 must not turn that into FFmpeg argv until M4 supplies the source
   layout and measured matrix. Source and sink sample-rate facts now fail
   closed before a copy is admitted; missing evidence takes the encode
   fallback.
4. **No feature gate or setting is added.** The server accepts and reports an
   evidence-bearing claim; from 2026-10-02 the Apple, Android and web clients
   send one. A Developer toggle
   would imply an enablement choice where the remaining boundary is physical
   evidence, not preference.
5. **Decoder, sink and passthrough are separate authority.** A sink codec does
   not populate the decoder set. Copy requires an explicit decoder plus a
   compatible sink, or a true passthrough claim plus a compatible sink; both
   require exact source/sink sample-rate agreement. *(Narrowed by decision 6:
   the rate rule applies to passthrough only.)*
6. **A decoding client's route does not refuse a copy (2026-10-02).** A
   sink's channel count is what the route reproduces and decides what the
   server encodes. A client that decodes the codec mixes any decodable layout
   down and resamples any rate for its own output — what it already does on a
   direct play — so copy and direct play require the channel count and the
   sample rate to fit only for a bitstream passed through undecoded, and a
   codec the claim does not mention (DTS, TrueHD, Opus) keeps the codec-list
   answer. Without this, the first claims would have turned 5.1 direct plays
   on stereo routes, DTS/TrueHD receiver passthrough, Opus WebM and odd-rate
   MP3/AAC into server remuxes with re-encoded audio. Server nodes must be
   updated before clients that send claims (an older server applies the
   strict rule).

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | Claim | [#418](http://forge.lan:3000/noirr/plurx/pulls/418) | Claimed `plan/S-09` from `665b8b5c`; M3–M5 remain evidence-gated. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M1 | [#418](http://forge.lan:3000/noirr/plurx/pulls/418) · `142502010` | Server sink claim, pure route negotiation and `delivered_audio` response implemented; 87 focused playback tests and the daemon serialization/translation/refusal checks passed. No client claim enabled. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M2 | [#418](http://forge.lan:3000/noirr/plurx/pulls/418) · `659fb6372` | Partial: lossy rolling outputs are fixed at 48 kHz. needs: normalized source channel-layout/sample-rate facts, a sink sample-rate contract and M4 matrix/clipping measurements before argv, identity, manifest, offline or prepared-handoff propagation. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M3–M5 | [#418](http://forge.lan:3000/noirr/plurx/pulls/418) | needs: the Apple/AVR/AirPods observations, per-layout loudness/peak/clipping measurements, then Android/web device evidence. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | Sole review disposition | [#418 comment #3296](http://forge.lan:3000/noirr/plurx/pulls/418#issuecomment-3296) | Separated decoder/sink/passthrough authority; source/sink sample rates now fail closed; recipe v4 keys the 48 kHz decision; flat `achannels` is bounded at the request; progressive AAC remux is fixed at 48 kHz. Hardware, layout and downmix evidence remain blocked honestly. |
| 2026-09-30 | gpt-6.1-sol | agent:/root/architecture_receipt_reconcile_sol61 | M2 source-layout prerequisite | Draft continuation on current effort `ffbbc16ce` | Claimed only optional source facts and backend round trips. `AudioStream.channel_layout` retains trimmed, opaque ffprobe spelling, bounded to 256 UTF-8 bytes; `5.1` and `5.1(side)` remain distinct. Empty, unknown/N/A, control-bearing and malformed non-string values mean no claim; unsupported bounded spellings remain opaque, not supported speaker maps. No inference from channel count. Absent/null legacy JSON remains absent on serialization, with no migration/backfill. Raw probe JSON retains rejected facts. Focused parser/serde and real SQLite plus feature-enabled three-voter Hiqlite round trips are required before push. No pan, gain, argv, recipe, client, runtime or surround-output change; M2 remains partial and M3–M5 acceptance remains owed. Current effort/focused-test workflow supersedes the older task-to-main/broad-suite instructions for this continuation. |
| 2026-10-01 | gpt-6.1-sol | agent:/root/s09_665_resume_sol61 | M2 propagation and sole-review disposition | [#665](http://forge.lan:3000/noirr/plurx/pulls/665) | Typed audio reaches actual producer options/argv, plan/recipe identity, manifests/budgets, remote responses, durable offline snapshots and prepared successors. Sole review #28/comment #6752 identified retained rolling/encoded audio being re-resolved; actual owner regressions fail with the old claim-first behavior and pass with retained authority. Initial rolling negotiation remains route-specific; legacy absence and the v4 golden remain pinned. Composed effort `ec79f4b34` / main `5c99538fd`: pinned workspace check, core7, daemon11, real SQLite/three-voter offline2, numeric/docs9 and ownership census passed on their recorded source. Later main `1b2ae4f62` (#662) is composed additively with its yield reasons, publication/admission ownership and tests; audio owner/test bytes are unchanged, measured census414. Current committed-tree/compiler/hook receipts are in the PR; no already-passed tests are repeated for this base refresh. Numeric failures and physical/content/listening limits are retained; M3–M5 remain open. |
| 2026-10-02 | claude-opus-5-5 | https://claude.ai/code/session_01CAyBrYCQ7PpAtuZwUxKfp7 | M4 real content + shipped stereo fold | Opus S-09 continuation PR | Nine real windows measured on the production ffmpeg; incumbent fold clipped and honoured AC-3/E-AC-3 stored levels. Float named matrix per layout + −4 dBFS limiter ships for every stereo fold (legacy absent-claim transcodes included); digest carries the exact chain; one `-af` with the A/V correction; encoded VOD folds ahead of the lattice trim (join bit-identical in PCM, lag 0). Listening notes and M3/M5 device evidence remain open. |
| 2026-10-02 | claude-opus-5-5 | https://claude.ai/code/session_01CAyBrYCQ7PpAtuZwUxKfp7 | M3/M5 client claims | Opus S-09 continuation PR | Apple (AVAudioSession route channels + HDMI receiver), Android (HDMI PCM channels + Media3 bitstreams) and web (AudioContext destination channels) send `audio_sinks`; decision 6 keeps decodable copies and direct plays for stereo routes. Focused Apple iOS+tvOS, Android JVM and web-policy tests pass; device observations owed. |
