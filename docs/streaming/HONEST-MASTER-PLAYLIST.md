# Honest master playlist — say what this session delivers, not what the file is

**Status:** M1–M2 landed; M3 implementation in progress, fleet/device acceptance open · **Executes:** Q7 / F-stream-14 / A11 /
F-apple-11 from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `0f02b7ea`

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Board id **S-10**. Companion to
[ADAPTIVE-QUALITY.md](ADAPTIVE-QUALITY.md) (what a rung is and what it
costs), [HEVC-SAMPLE-ENTRY-IMPLEMENTATION.md](HEVC-SAMPLE-ENTRY-IMPLEMENTATION.md)
(how the HDR master already reads its codec string out of `init.mp4`), and
[../playback-control/QUALITY-SWITCH-CONTINUITY-PLAN.md](../playback-control/QUALITY-SWITCH-CONTINUITY-PLAN.md)
§1.1 (why this master has exactly one `#EXT-X-STREAM-INF` and will keep
having one).

Read §2 in full before touching `hls.rs`. The master is not a cosmetic
header: AVPlayer filters variants on `BANDWIDTH` and `CODECS` *before it
fetches a byte*, and a master that over-claims is how a playable stream
becomes an unplayable one. Every milestone below makes one attribute true
and proves it on a device; none of them adds a second variant. **If a step
seems to require emitting more than one `#EXT-X-STREAM-INF`, changing
`should_serve_high_tier_media_playlist`, changing the HDR branch's existing
`VIDEO-RANGE`/`SUPPLEMENTAL-CODECS` rules, or loosening
`FrozenHlsPresentation`'s fingerprint, stop and flag it.** Line numbers are
from `0f02b7ea`; re-verify at build time by function name — this tree moves
daily.

**Correction to the review (three, all in your favour):**

1. **The encoded-VOD path already advertises output geometry.**
   `TranscodeManager::hls_presentation_before`
   ([transcode.rs:25082-25097](../../crates/plurxd/src/transcode.rs))
   overwrites `file.width`, `file.height`, `file.hdr`, `file.bit_depth`
   and `file.video_codec` with the *encoding's* values before the master is
   built. Q7 / F-stream-14's "`RESOLUTION={file.width}x{file.height}`
   regardless of `SessionKind::Transcode`" is true only for the **rolling**
   path, which freezes the source row unchanged
   ([transcode.rs:16347-16358](../../crates/plurxd/src/transcode.rs)).
   `file.bitrate` is **not** overwritten on either path, so `BANDWIDTH` is
   wrong everywhere. Plan against that split: geometry is one milestone for
   one path, bandwidth is one milestone for both.
2. **The hard-coded string is `avc1.640034`, not `avc1.640028`.**
   `transcoded_hls_codecs`
   ([transcode.rs:27610-27617](../../crates/plurxd/src/transcode.rs))
   answers `"avc1.640034,mp4a.40.2"` for `OutputGrade::Sdr`. `0x34` is
   level 5.2, not 4.0. The assessment's instruction ("do not hard-code
   avc1.640028 for every rolling rendition") stands with the literal
   corrected; the over-claim is larger than the review thought.
3. **The Apple panel mis-grades on a conjunction, not on the ratio alone.**
   `PlayerView.networkTone`
   ([PlayerView.swift:3261-3271](../../clients/apple/Sources/PlayerView.swift))
   requires `delivered < expected * 0.55 **&& runway < 2**` for `.critical`.
   A transcode with a full buffer grades `.good` today despite a 10x wrong
   `expected`. What the wrong number actually does is paint `.critical` on
   every transcode *start*, every seek and every dip — the moments a viewer
   is already looking at the panel. §6's reproduction targets those windows,
   not steady state. F-apple-11's "paints critical on a perfectly healthy
   delivery" is right about the defect and wrong about when it shows.

## 1. Objective

1. For a transcode session the multivariant playlist states this session's
   own delivered picture: `RESOLUTION` from the rung's resolved output
   geometry on **both** the rolling and the encoded-VOD path.
2. `CODECS` names what the bytes actually are — read from the initialization
   segment for fMP4 sessions, and derived from the argument list that
   produced the bytes for MPEG-TS sessions — and is emitted on SDR variants
   only after the recorded "SDR masters carry no CODECS" ruling has been
   re-qualified on the named devices (§3.3, §5.4).
3. `BANDWIDTH` is a peak the stream will not exceed and `AVERAGE-BANDWIDTH`
   is a separate, smaller number, both justified per RFC 8216 §4.3.4.2 and
   both checked against measured segment sizes on the `scripts/bench`
   corpus.
4. The Apple playback panel's "network" tone and the `indicated_bitrate_bps`
   field on every stall beacon become comparable to delivery, and the
   before/after is reproduced on a device.

Board id S-10. All milestones are logical commits and evidence rows in one
draft plan PR into `main`, per the work-board protocol.

## 2. Contract today

Re-verify line numbers at build time; they are from `0f02b7ea`.

### 2.1 The one variant, and where each attribute comes from

`master_playlist_with_shape`
([hls.rs:12399-12547](../../crates/plurxd/src/http/hls.rs)) emits exactly
one `#EXT-X-STREAM-INF` followed by the literal `index.m3u8`. The variant
block, verbatim:

```rust
    let bandwidth = file.bitrate.unwrap_or(25_000_000).max(128_000);
    out.push_str(&format!(
        "#EXT-X-STREAM-INF:BANDWIDTH={bandwidth},AVERAGE-BANDWIDTH={bandwidth}"
    ));
    if let (Some(width), Some(height)) = (file.width, file.height) {
        if width > 0 && height > 0 {
            out.push_str(&format!(",RESOLUTION={width}x{height}"));
        }
    }
    if let Some(frame_rate) = context
        .frame_rate
        .filter(|rate| rate.is_finite() && *rate > 0.0)
    {
        out.push_str(&format!(",FRAME-RATE={frame_rate:.3}"));
    }
```

(`hls.rs:12481-12496`.) `CODECS` follows, and only inside the HDR branch:

```rust
    let video_codec = context.codecs.split(',').next().unwrap_or_default();
    let hevc = ["hvc1", "hev1", "dvh1", "dvhe"]
        .iter()
        .any(|prefix| video_codec.starts_with(prefix));
    let video_range = if hevc { /* file.hdr -> PQ | HLG */ } else { None };
    if let Some(video_range) = video_range {
        if shape.video_range { out.push_str(&format!(",VIDEO-RANGE={video_range}")); }
        if shape.codecs { out.push_str(&format!(",CODECS=\"{}\"", quoted(&context.codecs))); }
        if shape.codecs { /* SUPPLEMENTAL-CODECS */ }
    }
```

(`hls.rs:12501-12528`.) So `MasterShape::codecs` defaults to `true`
(`hls.rs:12347-12355`) and is still never reached for an SDR session: the
`CODECS` writes are nested inside `if let Some(video_range)`, which is
`None` for every `avc1` variant. `context.codecs` is populated for those
sessions — it is simply not printed.

`CLOSED-CAPTIONS=NONE` is unconditional and stays so; its comment
(`hls.rs:12530-12540`) explains why.

### 2.2 Where `file` and `context` come from — three different answers

| Session | `file` fields | `context.codecs` | Source |
|---|---|---|---|
| Encoded VOD (fMP4) | width/height **replaced** by `output_size(&file, height)`; `hdr`/`bit_depth`/`video_codec` replaced by the grade; `bitrate` untouched | `transcoded_hls_codecs(grade, height)` | `transcode.rs:25078-25116` |
| Copy / remux VOD | source row verbatim | `copied_hls_codecs(...)` from the stored `probe_json` | `transcode.rs:25118-25157` |
| Rolling transcode (MPEG-TS) | source row verbatim, **including width/height/bitrate** | `transcoded_hls_codecs(grade, height)` | `transcode.rs:16342-16358` |

`SessionKind::Transcode { height }` (`transcode.rs:11000-11003`) is the
session's own rung. The rolling freeze even builds it — `cached_kind` at
`transcode.rs:16342` — and then does not use it to shape the file.

`transcoded_hls_codecs` (`transcode.rs:27602-27617`):

```rust
fn transcoded_hls_codecs(grade: OutputGrade, target_height: i64) -> String {
    match grade {
        OutputGrade::Sdr => "avc1.640034,mp4a.40.2".to_owned(),
        OutputGrade::Hdr10 if target_height > HDR10_HEIGHT => {
            format!("{HDR10_4K_HLS_CODEC},mp4a.40.2")
        }
        OutputGrade::Hdr10 => format!("{HDR10_HLS_CODEC},mp4a.40.2"),
    }
}
```

The two HDR constants are **measured** — `HDR10_HLS_CODEC =
"hvc1.2.4.H120.90"` and `HDR10_4K_HLS_CODEC = "hvc1.2.4.H150.90"`, each
with the hvcC field extraction that produced it recorded above it
(`transcode.rs:27580-27600`). The SDR arm is not measured. It claims High
profile (`0x64`), no constraint flags (`0x00`), level 5.2 (`0x34`) for
every SDR transcode including the 360p rung, on every encoder family.

### 2.3 What the encoder actually produces, per family

`Encoder::encode_args_for`
([encoder.rs:384-479](../../crates/plurx-core/src/transcode/encoder.rs)):
only `Encoder::Software` appends `-profile:v high`
(`encoder.rs:424-425`). `Nvenc`, `VideoToolbox`, `Vaapi` and `Qsv` set
neither `-profile:v` nor `-level` and leave both to the driver. No family
sets `-level` at all. This is already on the record —
[OFFLINE-VIEWING-REVIEW.md](../clients/OFFLINE-VIEWING-REVIEW.md) S4:
"`CODECS="avc1.640034"` is an unsafe hardcode … Derive `CODECS` from the
produced bitstream, or pin `-profile:v high -level 4.0` across encoders
first."

The rolling muxer is MPEG-TS with no initialization segment
(`hls_args_inner`,
[mod.rs:1688-1706](../../crates/plurx-core/src/transcode/mod.rs):
`-hls_segment_type mpegts`), which is why `transcoded_hls_codecs`'s doc
comment says the string "is the grade's, and the grade is the pipeline's".
The encoded-VOD path is fMP4 and does have an `init.mp4`.

### 2.4 The init-segment reader exists, and refuses AVC

`exact_hls_context_at`
([hls.rs:11720-11732](../../crates/plurxd/src/http/hls.rs)) returns the
context unchanged unless the first codec token starts with `hvc1`, `hev1`,
`dvh1` or `dvhe`:

```rust
    let fallback_video = context.codecs.split(',').next().unwrap_or_default();
    let Some(sample_entry) = ["hvc1", "hev1", "dvh1", "dvhe"]
        .into_iter()
        .find(|entry| fallback_video.starts_with(entry))
    else {
        return Ok(context);
    };
```

For HEVC it reads `init.mp4` under the response deadline, bounded by
`INIT_INSPECTION_LIMIT_BYTES`, and normalises the tier via
`hevc_codec_from_init` / `dolby_vision_hls_config`. The machinery — object
naming through `session_init_object`, the bounded read, the typed
pending/invalid/unavailable errors — is entirely reusable for `avc1`.

### 2.5 The recorded ruling this plan must re-qualify, not overwrite

Q7 says "the recorded 'SDR masters without CODECS' hardware ruling must be
re-qualified, not overwritten." Here is everything the tree records, so the
re-qualification knows what it is arguing with:

1. **A wrong `CODECS` is worse than none, observed.** `exact_hls_context_before`'s
   doc (`hls.rs:11559-11575`): "AVPlayer rejected exactly that on a
   Profile 5 title while the same session's media playlist, which carries
   no `CODECS` at all, played." The rejection is at asset preparation,
   before any byte transfers.
2. **`SUPPLEMENTAL-CODECS` costs a compatibility version, and version 10
   broke Apple.** `hls.rs:12414-12424`: advertising it from a version-7
   master "makes AVPlayer reject the otherwise valid Profile 8.1/8.4
   rendition during item preparation, and the Apple client then takes its
   final H.264/SDR compatibility fallback." Hence `compatibility_version`
   is 10 only when `shape.codecs && context.supplemental_codecs.is_some()`.
   **An SDR `CODECS` must not move the version.**
3. **A test pins the omission.**
   `the_hdr10_rungs_master_gets_pq_and_hevc_codecs_from_the_existing_rule`
   (`hls.rs:27913-27937`) ends with
   `assert!(!sdr.contains("CODECS="), "{sdr}")`. Changing that assertion is
   the visible half of re-qualifying the ruling; the device run in §5.4 is
   the other half.
4. **The copied-audio label is knowingly wrong and currently harmless.**
   `copied_audio_codec` (`transcode.rs:9885-9905`) answers `mp4a.40.2` for
   anything outside its five arms, "which **mislabels a genuinely copied
   FLAC, Opus or DTS track**", and the doc says it is left unfixed because
   "the native master carries no `CODECS` attribute …, so the wrong label
   is currently written to nothing." **Emitting `CODECS` on SDR masters
   publishes that lie.** It is why M3 fixes `copied_audio_codec` in the
   same PR that starts printing the string — the doc comment asks for
   exactly that ("it wants doing in the same change as whatever starts
   reading the result").
5. **Capability filtering can reject the only track.**
   [OFFLINE-VIEWING-REVIEW.md](../clients/OFFLINE-VIEWING-REVIEW.md) S4
   names API 23 Android devices where `MediaCodec` capability filtering can
   refuse a variant whose declared level exceeds what the decoder
   advertises. With one variant in the master there is nothing to fall back
   to.

### 2.6 Who reads the number, on the Apple side

`PlayerController`
([PlayerController.swift:2408-2414](../../clients/apple/Sources/PlayerController.swift)):

```swift
    var observedBitrate: Double? {
        player.currentItem?.accessLog()?.events.last?.observedBitrate
    }

    var indicatedBitrate: Double? {
        player.currentItem?.accessLog()?.events.last?.indicatedBitrate
    }
```

`indicatedBitrate` is AVFoundation's reading of the variant's advertised
`BANDWIDTH`. It reaches two places: the panel's tone
([PlayerView.swift:3261-3271](../../clients/apple/Sources/PlayerView.swift))
and the diagnostic snapshot that stall beacons carry as
`indicated_bitrate_bps` (`PlayerController.swift:398, 415, 2504`). The
Apple client always requests the master — it asks for native subtitles, so
`master_playlist_response` is its playlist.

### 2.7 The fingerprint that must move with the master

`FrozenHlsPresentation::new`
([transcode.rs:5616-5641](../../crates/plurxd/src/transcode.rs)) hashes the
inputs of the master:

```rust
        let identity = serde_json::json!({
            "version": 1,
            "file": &file,
            "kind": kind,
            "start_seconds": context.start_seconds,
            "media_origin_seconds": context.media_origin_seconds,
            "codecs": &context.codecs,
            "supplemental_codecs": &context.supplemental_codecs,
            "frame_rate": context.frame_rate,
        });
        let contract_fingerprint = hex::encode(Sha256::digest(identity.to_string().as_bytes()));
```

and seals a stable-master contract only when the first codec token is not
HEVC/DV, because those masters may still be normalised from `init.mp4`
(`transcode.rs:5630-5635`). Two consequences bind every milestone:

- **Any new master input must enter this JSON.** If a rung's geometry or a
  peak bandwidth is carried on `HlsContext` rather than on `file`, and it
  is not in `identity`, two different masters share one fingerprint and the
  response-publication contract (`bind_response_publication_contract`,
  `transcode.rs:16362`) stops distinguishing them.
- **Making the AVC string init-derived un-seals the stable contract.** The
  `master_requires_attempt_init` predicate is a prefix test on the codec
  token; adding `avc1` to it moves fMP4 AVC masters from generation
  metadata to attempt media. That is a real behaviour change in
  `commit_authorized_media` (`transcode.rs:24308-24322`) and it is why M2
  is its own PR with its own tests.

```text
  master.m3u8 request (Apple, native subtitles)
        |
        v
  session_file ---> hls_presentation_before -------+
        |                                          |
        |   VOD encoded: file reshaped by rung     |  rolling: source row
        |   copy: source row + probe codecs        |  verbatim
        v                                          |
  exact_hls_context_before  (HEVC/DV only today) <-+
        |   reads init.mp4, normalises tier/profile
        v
  master_playlist_with_shape
        |   BANDWIDTH/AVERAGE-BANDWIDTH <- file.bitrate   (both wrong)
        |   RESOLUTION                  <- file.w/h       (wrong: rolling)
        |   CODECS                      <- context.codecs (HDR branch only)
        v
  one #EXT-X-STREAM-INF + "index.m3u8"
```

## 3. Change

### 3.1 Geometry from the rung, on both paths

The rolling freeze already knows the rung. Reshape the frozen file the same
way the VOD path does, at `transcode.rs:16342-16358`, before
`FrozenHlsPresentation::new`:

```rust
        let cached_kind = SessionKind::Transcode { height: opts.target_height };
        let mut presented = file.clone();
        if let Some((w, h)) = plurx_core::transcode::output_size(&file, opts.target_height) {
            presented.width = Some(w);
            presented.height = Some(h);
        }
```

`output_size` ([mod.rs:927-937](../../crates/plurx-core/src/transcode/mod.rs))
never upscales and returns even dimensions at the source aspect; `None`
means the source was never probed, in which case the master keeps omitting
`RESOLUTION` exactly as it does today. Do **not** reshape `hdr`,
`bit_depth` or `video_codec` on the rolling path in this milestone — the
VOD path does it because its grade is decided at recipe capture, and doing
it here would change the `VIDEO-RANGE` decision, which is out of scope.

The other rolling freeze sites (`transcode.rs:21292`, `:22050`, `:28617`)
take the same treatment; they build the same structure for the live and
takeover producers. `refresh_frozen_presentation_from_store`
(`transcode.rs:28202-28229`) rebuilds from the store row and must apply the
same reshape, or a probe refresh silently reverts the master mid-session.

Copy sessions are untouched: the advertised geometry *is* the source's,
because the bytes are the source's.

### 3.2 An exact codec string, from the bytes for fMP4

Extend `exact_hls_context_at` to `avc1`. The sample-entry list becomes
`["hvc1", "hev1", "dvh1", "dvhe", "avc1"]`. The AVC identity comes from a
structural walk through the parsed video track's
`trak/mdia/minf/stbl/stsd/avc1|avc3/avcC` chain, never from a raw `avcC` byte
occurrence. A `free`/`uuid` payload is irrelevant. Multiple descriptions are
accepted only when their sample-entry fourcc and three-byte identity agree;
mixed or disagreeing descriptions are an unsupported layout. The record is:

```text
  avcC: configurationVersion(1) AVCProfileIndication profile_compatibility
        AVCLevelIndication ...
  CODECS := format!("avc1.{:02X}{:02X}{:02X}",
                    profile_indication, profile_compatibility, level_indication)
```

The selected entry's actual `avc1`/`avc3` fourcc prefixes the result. The same
`HlsInitInspectionError` pending/invalid/unavailable/unsupported split applies
to a short, malformed or ambiguous init, and the valid-init tests carry an
earlier decoy plus agreeing and disagreeing duplicate descriptions.
The existing `INIT_INSPECTION_LIMIT_BYTES` bound applies unchanged.

The fallback when the init cannot be read stays what it is today for HEVC:
the context's own string. What that string *is* for AVC is §3.3's problem.

### 3.3 A true static string for MPEG-TS, per family and rung

The rolling path has no init to read, so the declaration must be made true
by the argument list instead of read from the output. Two coupled changes,
both in `plurx-core`:

1. **Force the profile and level on every SDR family.** In
   `encode_args_for`'s SDR arms, append `-profile:v high` for `Nvenc`,
   `Qsv`, `Vaapi` and `VideoToolbox` as `Software` already does, and append
   an explicit `-level` chosen from the rung, per the table below. No
   family gets a flag its driver refuses — §5.3's probe decides that before
   the flag ships, and a family that refuses keeps today's argv **and**
   today's declaration-free master.
2. **Derive the string from rung and cadence.** The safe candidate below is
   deliberately sized for every resolved cadence up to and including 60 fps;
   a 23.976/30 fps observation cannot qualify it. A source above 60 fps stays
   declaration-free until its own Annex A bound and encoder evidence exist.

   | Rung (`output_size` height) | Macroblocks/frame | MB/s at 60 fps | H.264 level | `CODECS` |
   |---|---:|---:|---|---|
   | 360 (640x360) | 920 | 55,200 | 3.1 (`0x1F`) | `avc1.64001F` |
   | 480 (854x480) | 1,620 | 97,200 | 3.1 (`0x1F`) | `avc1.64001F` |
   | 720 (1280x720) | 3,600 | 216,000 | 3.2 (`0x20`) | `avc1.640020` |
   | 1080 (1920x1080) | 8,160 | 489,600 | 4.2 (`0x2A`) | `avc1.64002A` |
   | 2160 (3840x2160) | 32,400 | 1,944,000 | 5.2 (`0x34`) | `avc1.640034` |

   These are still **proposed** values, not fleet evidence. They correct the
   earlier 30-fps-like table by applying H.264 Annex A MaxMBPS to the coded
   macroblock dimensions. Qualification runs 23.976, 29.97, 59.94 and 60 fps
   at every affected rung/family and reads the emitted SPS `level_idc`.
   A driver that refuses the flag or rounds up keeps today's argv and a
   declaration-free master. If accepted values differ by family or cadence,
   the identity and declaration become a function of `(Encoder, height,
   resolved frame grid)`; they are never selected from height alone.

The constraint-flags byte stays `0x00`: High profile with no constraint set
is what `-profile:v high` produces, and a nonzero byte would be a claim
about `constraint_set*_flag` nobody has read.

`bitrate_for_height` (`transcode.rs:27519-27526`) is the only other place
the rung's identity turns into numbers; nothing in this plan changes it.

**This changes recipe identity for measured cells only.** The resolved plan's
digest conditionally includes the measured codec and rational output grid
when a successful exact-family experiment admits profile/level flags.
Qualified hardware entries change, and qualified software entries also change
because their explicit level is new even though High profile was already
present. Untested/refused cells keep the incumbent arguments and digest.
Frozen codec metadata follows that same immutable plan; no universal triplet
is inferred from a height.

### 3.4 Peak and average, separately, with the overhead named

RFC 8216 §4.3.4.2: `BANDWIDTH` "MUST be the peak segment bit rate of the
Variant Stream", computed as the size of a media segment in bits divided by
its `EXTINF` duration, maximised over all segments. `AVERAGE-BANDWIDTH`, if
present, is the same quantity averaged rather than maximised. Both are of
the *segment*, so both include container bytes; neither is the encoder's
target.

`Rung` already carries both halves
([transcode.rs:27676-27688](../../crates/plurxd/src/transcode.rs)), and its
doc comment already says what they are for:

```rust
pub struct Rung {
    pub height: i64,
    /// The rung's nominal cost on the wire: video target + audio, in kb/s.
    pub total_kbps: u32,
    /// What the rung may PEAK at over the rate-control window: `-maxrate`
    /// (1.5x the target, PERF-PLAN §4.6) + audio. The number that must
    /// cover the measured burst -- media1 measured 9.05 Mb/s on the 1080
    /// rung's 8 Mb/s target -- and the one an HLS `BANDWIDTH` attribute
    /// would be required to state.
    pub peak_kbps: u32,
}
```

The declaration adds the container term the struct does not model:

```text
  AVERAGE-BANDWIDTH = (video target + audio bitrate) * (1 + overhead)
  BANDWIDTH         = (video maxrate + audio bitrate) * (1 + overhead)

  video maxrate = video target * 3 / 2        (encoder.rs:395)
  audio bitrate = opts.audio_bitrate_kbps     (mod.rs:893, default 160)
  overhead      = measured, per container (SEGMENT_CONTAINER_OVERHEAD)
```

MPEG-TS carries 184 payload bytes per 188-byte packet, so its floor is
2.17 % before PAT/PMT/PCR/adaptation padding; fMP4's `moof`+`mdat` framing
is well under 1 %. Both numbers are **measured** in §5.5 across the bench
corpus rather than asserted, and the constant lands with the measurement in
its doc comment, the way `HDR10_HLS_CODEC` did.

Two guardrails the assessment demands (F-stream-14, F-apple-11): "configured
maxrate plus audio is not automatically a measured HLS peak, especially over
short segments", and "distinguish average bitrate". §5.5's acceptance is
therefore `max(segment bytes * 8 / EXTINF) <= BANDWIDTH` over every segment
of every corpus fixture at every rung; if a 2 s segment bursts past
`maxrate + audio + overhead`, the declared peak rises to the measurement and
the plan says so in the PR body rather than quietly clamping.

Copy/remux sessions must not call `file.bitrate` or ffprobe `max_bit_rate` a
measured peak. **2026-10-01 source correction:** the existing fragment index
contains video-only pipe lengths, not full selected audio/container output.
It cannot provide the formerly proposed exact full-wire bound. The phased
M5 implementation therefore observes successful full-mux VOD materialization
and complete retained coverage (§5.5). A prepared successor captures its own
compatible complete artifact receipt; a shared source or video rung is not
enough. Arbitrary cold-copy first-publication remains open rather than using
a prefix maximum, scaled average or guessed overhead. Legacy publication is
not thereby claimed honest.

### 3.5 Where the numbers live

`HlsContext` (`transcode.rs:9677-9689`) gains two fields, both
`Option<u32>` in kb/s, both `None` for copy sessions in M1:

```rust
pub struct HlsContext {
    // ... existing fields ...
    /// Peak segment bit rate this session will not exceed, kb/s, container
    /// included. `None` means "no better answer than the source row".
    pub peak_kbps: Option<u32>,
    /// Average segment bit rate, kb/s, container included.
    pub average_kbps: Option<u32>,
}
```

Both go into `FrozenHlsPresentation::new`'s `identity` JSON (§2.7) in the
same PR, and `identity`'s `"version"` goes from 1 to 2 so a fingerprint
computed under the old shape can never collide with one under the new.

`master_playlist_with_shape` then reads them, falling back to today's
expression when both are `None`:

```rust
    let average = context.average_kbps.map(|kbps| u64::from(kbps) * 1000)
        .unwrap_or_else(|| file.bitrate.unwrap_or(25_000_000).max(128_000) as u64);
    let peak = context.peak_kbps.map(|kbps| u64::from(kbps) * 1000).unwrap_or(average);
    out.push_str(&format!(
        "#EXT-X-STREAM-INF:BANDWIDTH={peak},AVERAGE-BANDWIDTH={average}"
    ));
```

### 3.6 Printing `CODECS` on SDR variants

Once §3.2 and §3.3 make the string true, the write moves out of the HDR
branch:

```rust
    if let Some(video_range) = video_range {
        if shape.video_range { out.push_str(&format!(",VIDEO-RANGE={video_range}")); }
    }
    if shape.codecs {
        out.push_str(&format!(",CODECS=\"{}\"", quoted(&context.codecs)));
        if let Some(supplemental) = context.supplemental_codecs.as_deref() {
            out.push_str(&format!(",SUPPLEMENTAL-CODECS=\"{}\"", quoted(supplemental)));
        }
    }
```

`compatibility_version` is unchanged: it already keys on
`shape.codecs && context.supplemental_codecs.is_some()`, and an SDR session
has no supplemental codecs, so the master stays at version 7 (§2.5 item 2).

This is the edit that needs §5.4's device run before it merges, and it is
last for that reason.

## 4. Guardrails (non-goals)

- **Still one variant.** A better-described variant is not a ladder.
  [QUALITY-SWITCH-CONTINUITY-PLAN.md](../playback-control/QUALITY-SWITCH-CONTINUITY-PLAN.md)
  §1.1: each rung is its own server session and a rung change is a new
  `POST`. Nothing here emits a second `#EXT-X-STREAM-INF`, and §3.8's
  native adaptive work
  ([../clients/NATIVE-ADAPTIVE-QUALITY-DESIGN.md](../clients/NATIVE-ADAPTIVE-QUALITY-DESIGN.md))
  does not depend on one appearing.
- **The HDR branch keeps every rule it has.** `VIDEO-RANGE` stays keyed on
  the `hvc1`/`hev1`/`dvh1`/`dvhe` prefix plus `file.hdr`;
  `SUPPLEMENTAL-CODECS` stays inside `shape.codecs`; the
  version-7-vs-10 rule (§2.5 item 2) is untouched. The review's positive
  assessment of those rules stands.
- **The ruling is re-qualified, not overwritten** (Q7's explicit
  requirement). §2.5 lists what it recorded; §5.4 re-runs the observation
  that produced it on the named devices; the pinning test changes only
  after that run reports.
- **No hard-coded profile/level survives.** F-stream-14: "Do not hard-code
  avc1.640028 for every rolling rendition: output profile/level vary."
  §3.2 derives from the bytes for fMP4 and §3.3 derives from an argv that
  is itself verified against the emitted SPS.
- **Target bitrate is not peak bandwidth** (F-stream-14, F-apple-11).
  §3.4 counts audio and a measured container term and validates against
  real segment sizes; the average is a separate attribute.
- **A better manifest does not measure the network** (F-apple-11). The
  panel's `.critical` rule is not retuned here. Fixing `expected` is the
  whole fix; if the tone still misreads after §5.6's reproduction, that is
  a separate finding against `PlayerView.networkTone`.
- **Copied streams, prepared replacements and frame rate keep working**
  (F-apple-11's "keep copied streams, prepared replacements and actual
  output resolution/frame rate correct"). Copy sessions are out of scope
  for geometry and codecs; the prepared-successor master is built through
  the same `hls_presentation_before`, so a reshaped file reaches it too and
  §5.1's test covers a prepared handoff explicitly; `FRAME-RATE` already
  comes from the encoding grid for VOD (`transcode.rs:25109-25112`) and
  from the frozen probe for rolling.
- **No feature gate.** Paul refuses in-code gates. Nothing here needs a
  switch: each milestone either makes an attribute true or does not ship.
  The existing `PLURX_HLS_FORCED_AUTOSELECT` rung (`hls.rs:12318-12323`) is
  not extended, and no new env rung is added.
- **`master_playlist_diagnostic`'s four shapes stay.** They are how a
  device run isolates an attribute (`hls.rs:12357-12387`), and §5.4 uses
  them.

## 5. Milestones

Each milestone is a logical commit in the one draft plan PR into `main`.

### 5.1 M1 — rolling geometry from the rung

Code: §3.1, at all four rolling freeze sites plus
`refresh_frozen_presentation_from_store`. No client change.

Tests, in `transcode.rs`'s existing module:

| Test | Asserts |
|---|---|
| `a_rolling_transcode_freezes_the_rung_geometry` | 3840x2160 source at the 720 rung freezes 1280x720; aspect preserved, both sides even |
| `a_rolling_transcode_of_an_unprobed_source_freezes_no_geometry` | `width`/`height` stay `None`; master omits `RESOLUTION` |
| `a_rolling_transcode_never_upscales_its_declaration` | 640x360 source at the 1080 rung freezes 640x360 |
| `encoded_vod_presentation_never_mixes_source_width_with_requested_height` | encoded VOD at a higher requested rung uses the one 640x360 `output_size` tuple, never 640x1080 |
| `encoded_vod_presentation_omits_both_unprobed_dimensions` | an unprobed encoded-VOD source carries neither coordinate |
| `a_probe_refresh_keeps_the_frozen_rung_geometry` | `refresh_frozen_presentation_from_store` after a store row change still reports the rung's geometry |
| `a_prepared_successor_declares_its_own_rung` | prepared 480 successor to a 1080 incumbent: the successor's master says 854x480 (or the source-aspect equivalent) |

Acceptance: `cargo test -p plurxd frozen_presentation rung_geometry` green;
`make unit` green; a manual `curl` of `master.m3u8` for a rolling 720
session on a 4K fixture prints `RESOLUTION=1280x720`.

Implemented in the shared `FrozenHlsPresentation` constructor so cached
starts, live starts, takeovers, prepared successors and test probe refreshes
cannot drift. The encoded-VOD resolver now applies that same coherent
`output_size` tuple to both dimensions before adding recipe facts; copy
sessions retain source geometry. Focused tests pin 4K-to-720 output, unprobed
omission, no upscaling on both transcode paths and unchanged copy dimensions.

### 5.2 M2 — the AVC string from `init.mp4`, for fMP4 sessions

Code: §3.2, plus adding `avc1` to `master_requires_attempt_init`
(`transcode.rs:5630-5634`) and the fingerprint version bump from §3.5 if
M3 has not landed it. `transcoded_hls_codecs`'s SDR arm is still the
fallback in this PR; M3 replaces it.

Tests:

| Test | Asserts |
|---|---|
| `avc_codec_from_init_reads_the_avcc_triplet` | valid parsed init returns its exact triplet and changing the selected entry to `avc3` changes the prefix |
| `avc_codec_from_init_ignores_an_earlier_decoy_box` | an earlier valid-looking `avcC` payload in `free` cannot become identity |
| `avc_codec_from_init_rejects_ambiguous_sample_descriptions` | matching duplicates agree; a different second triplet is unsupported |
| `avc_codec_from_init_refuses_a_truncated_box` | truncated structural init is malformed, not a partial string |
| `an_fmp4_avc_session_normalises_its_codec_from_the_init` | `exact_hls_context_at` returns the init's string, not `transcoded_hls_codecs`'s |
| `an_fmp4_avc_session_refuses_ambiguous_sample_descriptions` | the publication path returns typed `hls_init_unsupported` for conflicting entries |
| `an_mpegts_session_keeps_its_static_codec_string` | rolling session: no init read attempted, context unchanged |
| `an_fmp4_avc_master_is_attempt_media_not_generation_metadata` | `sealed_stable_master_contract` is `None` for an `avc1` context |

Acceptance: `cargo test -p plurxd avc_codec_from_init exact_hls_context`
green; `make unit` green; on a dev server, an encoded-VOD session's
`master.m3u8` fetched twice returns the same string and a
`grep session_init_object` of the logs shows exactly one init read per
session.

Implemented with a bounded `avcC` parser and the existing init-publication
deadline. AVC inspection is owner-scoped: encoded VOD and rolling copy use
fMP4 init bytes, while a rolling full transcode keeps its static MPEG-TS
answer and never waits for an init object it cannot produce. AVC fMP4 masters
are attempt media, and the frozen-presentation fingerprint is version 2 so
the changed publication contract cannot collide with the older shape.

### 5.3 M3 — qualified profile/level/cadence, and a frozen output string

Code: §3.3, both halves. This is the recipe-identity PR; the body states
which cached entries invalidate and why.

Per-family qualification, which gates the flag rather than following it:
extend the existing boot validation
(`detect_encoders` / `validate`,
[encoder.rs:1137-1197](../../crates/plurx-core/src/transcode/encoder.rs))
with a probe encode carrying `-profile:v high -level <rung>` for each
usable hardware family, and record the verdict beside `quality_rc` in
`EncoderCaps` (`encoder.rs:631-640`). A family whose probe refuses keeps
today's argv and stays on the declaration-free master — that is what makes
this a qualification and not a flag edit.

Tests:

| Test | Asserts |
|---|---|
| `only_exact_qualified_plans_change_encoder_flags_and_recipe_identity` | Only measured family/raster/cadence/rate-control cells gain High/level flags; unsupported or refused cells retain their original argv |
| `the_declared_level_covers_the_resolved_grid` | 360/480/720/1080/2160 at 59.94/60 map to §3.3; >60 stays unqualified |
| `a_family_that_refused_the_probe_keeps_its_old_arguments` | caps with the new verdict false -> argv identical to `0f02b7ea`'s |
| `the_recipe_hash_changes_for_a_family_that_gained_the_flags` | qualified flags change the digest, including software when it gains an explicit level; unqualified argv and digest remain unchanged |

Bitstream acceptance — the level in the argv must equal the level in the
SPS. Per enabled family on a node that has it, for every rung at 23.976,
29.97, 59.94 and 60 fps:

```bash
# one 20 s rolling session per rung, then read the SPS the encoder emitted
ffprobe -v error -select_streams v:0 \
  -show_entries stream=profile,level,width,height \
  -of default=nw=1 seg00003.ts
```

`level` must equal the declared `0xLL` as a decimal (e.g. `42` for `0x2A`),
`profile` must be `High`, and `width`x`height` must equal the master's
`RESOLUTION`. A mismatch on any family replaces that family's row in the
§3.3 table with the measured value before the PR merges.

Acceptance: `cargo test -p plurx-core encode_args_for level` and
`cargo test -p plurxd transcoded_hls_codecs` green; `make unit` green; the
ffprobe table above filled for every family the fleet actually selects (see
the GPT prompt in §6).

### 5.4 M4 — re-qualify the SDR ruling on the named devices, then print `CODECS`

**2026-10-01 ordering ruling:** implementation may be prepared and integrated
on the effort branch before physical-device qualification. This does not
waive the device acceptance below or qualify a release. The bounded M4
implementation freezes explicit video and audio component provenance;
unknown components do not gain a fabricated AAC or universal AVC label.
Qualified encoder output or the actual M2 AVC init supplies video identity;
the resolved output-audio decision supplies audio identity, including proven
absence. SDR `CODECS` is emitted only when both components are complete.
The copied codec name `aac` alone does not prove AAC-LC rather than HE-AAC;
without a frozen output AudioSpecificConfig it remains incomplete. Resolved
AAC encoding retains the native encoder's established output contract.
HDR declarations, variant topology and prepared-owner identity are retained.
There is no runtime gate or temporary diagnostic switch.

The required qualification run uses
`master_playlist_diagnostic`'s existing shapes
(`?diagnostic=video-only`, `video-only-codecs`) so one attribute changes at
a time, against a build carrying M2 and M3 but with §3.6 not yet applied
(the diagnostic shape reaches the `shape.codecs` branch; a temporary
diagnostic value `sdr-codecs` that emits `CODECS` outside the HDR branch is
acceptable and is deleted in the same PR that makes it unconditional).

What the run must answer, because §2.5 is what it is arguing with:

1. Does AVPlayer accept a version-7 SDR master carrying
   `CODECS="avc1.<measured>,mp4a.40.2"` on tvOS and iOS — at item
   preparation, on first play, and after a prepared handoff?
2. Does Media3 accept the same master on the three televisions, including
   an API-23-class device if one is reachable?
3. Does hls.js accept it in Safari, Chrome and Firefox?
4. With a copied FLAC/Opus/DTS audio track, does the `copied_audio_codec`
   fix (§2.5 item 4) produce a string those players accept — and does the
   *unfixed* string produce the rejection the doc predicts? Both halves,
   because the second is the evidence that the fix was needed.

Then: §3.6, plus the `copied_audio_codec` arms for `flac`, `opus`, `dts`
and `truehd`, plus flipping
`the_hdr10_rungs_master_gets_pq_and_hevc_codecs_from_the_existing_rule`'s
final assertion from `!sdr.contains("CODECS=")` to an exact-string check,
with a comment naming this document and the date of the device run — so
the next reader finds the re-qualification, not a deleted assertion.

Acceptance: the four questions answered in §6's results table with device
and OS versions; `cargo test -p plurxd master_playlist` green;
`mediastreamvalidator` (§6) reports no `CODECS`-related error on one SDR
and one HDR master.

### 5.5 M5 — peak and average, measured

Code: §3.4 and §3.5.

**2026-10-01 phased implementation ruling and arithmetic correction:** start
with one bounded metadata reducer and the existing VOD sink's successfully
materialized full mux bytes. Complete coverage means every immutable entry,
including audio tails, with the exact output identity. A complete retained
artifact receipt can supply a compatible **new** frozen presentation; later
observations must never rewrite an issued master or update a successor from
its predecessor. Partial coverage, conflicting duplicates, invalid durations,
gaps or checked-arithmetic overflow remain unknown, not a measured bound.
No duplicate payload buffers or new scheduler are needed.

**2026-10-01 actual-source retention boundary:** the bounded reducer and
successful full-mux VOD observer are implemented; their completed-output
notification comes only after a real trailer and successful final/tail writes,
not `Outcome::Ran` (which also describes a killed pipe). Observations use the
exact six-decimal playlist durations and source/recipe/init/execution identity.
Metadata is capped at 8192 entries and 131072 examined RFC windows. Adopted
unobserved objects, duplicate publication or a changed execution stay unknown;
ordinary legacy playback is not refused because measurement is unavailable.

The frozen **consumer remains open**. Failed renditions can be replaced under
the same recipe directory, and older session GETs still open that directory's
paths. A recipe match or collector nonce cannot make replacement bytes part
of an older measured artifact. Before exposing a complete-retained receipt,
bind GETs and replacement to a proven generation-distinct artifact retention
boundary, then capture the eligible receipt once at new-session attachment.
Earlier sessions keep None. Contradictory regeneration must be refused before
mutation without destroying the incumbent or rewriting its master. Until that
seam is proved, the observer does not publish measured bandwidth on the wire.
This is a finite implementation remainder, not device evidence alone or a
reason to reject cold titles. Existing normalized predictive classes remain
unchanged and must not be relabelled as measured title output.

RFC 8216 §4.1 defines peak over contiguous windows whose total duration is
0.5–1.5 target durations. Average is total media bits divided by total wire
`EXTINF` duration, not the unweighted mean of segment rates. The stronger
original per-segment burst acceptance remains a separate measurement.
The sample shell calculation below is a legacy burst diagnostic, not the RFC
peak or duration-weighted average algorithm.

Rolling-copy/PUT collection and arbitrary cold-copy exact first-publication
bounds remain a finite follow-up, explicitly open. A prefix, video-only index,
nominal encoder rate or guessed container overhead cannot prove them. Complete
background preparation costs full-source I/O and remux work (plus an audio
encode when selected); retaining the actual completed rendition avoids the
separate byte-count/boundary regeneration proof. No hidden whole-film startup
wait, forced transcode or rejection of playable titles is authorized. Device,
corpus, fetched-wire equality and prepared-successor acceptance remain open.

Measurement protocol, on media1 against the `scripts/bench` corpus
(`scripts/bench fixtures` builds it; the fixtures are `1080p-h264`,
`4k-hevc-sdr`, `4k-hdr10`, `4k-hlg`, `sparse-gop`, `grainy` —
`scripts/bench:55-99`). For each fixture at each rung, open a session,
fetch every segment, and compute the per-segment bitrate against its
`EXTINF`:

```bash
# per-segment peak and average for one session's playlist
BASE=http://media1:32400; SESSION=<session id>
curl -s "$BASE/hls/$SESSION/index.m3u8" > /tmp/idx.m3u8
awk '/^#EXTINF:/{d=substr($0,9)+0; next} !/^#/{print d, $0}' /tmp/idx.m3u8 |
while read -r dur seg; do
  bytes=$(curl -s -o /dev/null -w '%{size_download}' "$BASE/hls/$SESSION/$seg")
  echo "$seg $dur $bytes $(( bytes * 8 / 1000 ))"    # kbit in the segment
done | awk '{r=$4/$2; s+=r; n++; if(r>m) m=r}
            END{printf "peak %.0f kb/s  avg %.0f kb/s  n=%d\n", m, s/n, n}'
```

Report, per fixture and rung: configured target, `maxrate`, audio bitrate,
measured average, measured peak, and the implied container overhead
(measured average divided by target+audio, minus one). `SEGMENT_CONTAINER_OVERHEAD`
takes the **maximum** observed overhead across the corpus, per container,
and the constant's doc comment records the corpus, the date and the node,
the way `HDR10_HLS_CODEC`'s does.

Run a second matrix over representative source-copy and remux sessions:
H.264/AAC fMP4 copy, HEVC/AAC fMP4 copy, an audio-remux case, and a prepared
copy successor after handoff. Include at least one sparse-GOP and one
high-bitrate source. Measure the output fragments named by each session's
playlist—not the source container—and compare the result with the fragment
index's `(bytes, duration)` peak and average. Record source container, output
container, whether audio was copied or transcoded, source `bitrate` and
`max_bit_rate`, indexed peak/average, fetched peak/average, and declared
attributes. A missing probe `max_bit_rate` is an ordinary required row, not a
reason to substitute the average.

Acceptance: declared `BANDWIDTH >= measured peak` for every segment of every
transcode fixture/rung and every copy/remux/handoff row; declared
`AVERAGE-BANDWIDTH` within +/-5 % of the measured average and never below it;
the fragment-index calculation equals the fetched copy/remux result; no copy
path falls back to `file.bitrate` as peak; `cargo test -p plurxd
master_bandwidth` green; `make unit` green; both tables in the PR body.

### 5.6 M6 — reproduce the Apple panel consequence, before and after

No code. One device session each side of M5, recorded in §6's table.

Reproduction recipe, which §2.6 and the correction at the top make exact:
the tone is `.critical` only while `delivered < expected * 0.55 && runway < 2`,
so the observable windows are start, seek and any dip.

1. On Apple TV, play a >=40 Mb/s 4K SDR fixture forced to the 720 rung
   (Settings -> quality -> 720p).
2. Open the playback info panel **before** pressing play, and record the
   "network" tone and the displayed indicated/observed bitrates at first
   frame, at +2 s, at +10 s, and immediately after a +5-minute seek.
3. Repeat with the M5 build.
4. Pull the stall beacons for both sessions from Settings -> Logs and
   record `indicated_bitrate_bps` on each.

Acceptance: before, the panel shows `indicated_bitrate_bps` within 5 % of
the source's overall bitrate and a `.critical` tone at first frame and
after the seek; after, `indicated_bitrate_bps` is within 10 % of the
rung's declared peak and the tone is not `.critical` in those windows for a
healthy link. Both recorded here under a dated heading.

## 6. Verification and rollout

Fast lane per PR: `make unit`. Focused, by milestone: `cargo test -p plurxd
frozen_presentation rung_geometry` (M1), `cargo test -p plurxd
avc_codec_from_init exact_hls_context` (M2), `cargo test -p plurx-core
encode_args_for level` (M3), `cargo test -p plurxd master_playlist` (M4),
`cargo test -p plurxd master_bandwidth` (M5). Web: `make web-check` is
unaffected by the server change but is run on M4 because hls.js parses the
new attribute.

Three validators, none of which a `cargo test` can stand in for:

- **`mediastreamvalidator`** (Apple's HLS Tools) against a live session's
  `master.m3u8` for: one SDR rolling session, one SDR encoded-VOD session,
  one HDR10 session, one copy session. It checks `BANDWIDTH` against the
  segments it fetches and `CODECS` against the media it parses, which is
  precisely M3's and M5's claim. Record the version of the tool.
- **hls.js** in Safari, Chrome and Firefox via the shipped web player, with
  the browser console captured — the existing `scripts/web-hls-startup-browser-check`
  harness drives the player and is the place to read the result.
- **Media3** on the three televisions, which only a device can run.

**GPT prompt — fleet encoder inventory and bitstream levels (M3):**

```text
On media1 and on every lab node that has a GPU, with the current plurxd
running:
1. `curl -fsS -H "Authorization: Bearer $TOKEN" http://<host>:32400/api/v1/system`
   and report, per node: the detected encoder pills (nvenc, qsv, vaapi,
   videotoolbox), the encoder the server says it will select, and the
   PLURX_HWACCEL preference.
2. For each node whose selected encoder is NOT software: start a 720p
   transcode session of each 4K SDR cadence fixture (23.976, 29.97, 59.94
   and 60 fps), wait 30 s, find the session's
   scratch directory under the transcode root, and run
   `ffprobe -v error -select_streams v:0 -show_entries
    stream=profile,level,width,height -of default=nw=1 <seg00003.ts>`.
   Report profile, level, width, height, and the node's encoder.
3. Repeat step 2 at the 360, 480, 1080 and 2160 rungs. If a node or family
   cannot produce a row, mark that exact family/rung/cadence unqualified;
   do not inherit the nearest cadence's result.
4. Also fetch that session's master playlist
   (`curl -s http://<host>:32400/hls/<session>/master.m3u8`) and paste the
   `#EXT-X-STREAM-INF` line beside each ffprobe row.
Report one table per node: cadence, rung, encoder, declared RESOLUTION,
actual width x height, declared CODECS, actual profile/level. Include one
>60 fps control if the fleet admits such a source and confirm it remains
declaration-free until separately qualified.
```

**GPT prompt — SDR CODECS device re-qualification (M4):**

```text
With a build carrying the S-10 M2+M3 changes deployed to media1:
1. Apple TV 4K (tvOS) and an iPhone: play a 1080p SDR transcode of
   "Harbor Lights" with native subtitles enabled. Record whether playback
   starts, time to first frame, and any AVFoundation error. Then repeat
   with `?diagnostic=video-only-codecs` appended to the master URL.
2. Repeat both on a title whose audio is copied FLAC (or Opus, or DTS) —
   a copy session, not a transcode.
3. Android TV (Lenovo / Google TV / Shield) and an Android phone: same two
   titles, same two master shapes. Record any ExoPlayer/Media3
   `MediaCodec` capability error.
4. Safari, Chrome and Firefox on the web player: same two titles, console
   captured.
5. If any reachable device is API 23-class, run step 3 there too and say so.
Report a table: device, OS version, master shape, started yes/no, time to
first frame, error text. Include the exact CODECS string each master
carried (paste the #EXT-X-STREAM-INF line).
```

**GPT prompt — Apple panel before/after (M6):** the recipe in §5.6, steps 1
to 4, run once per build.

Rollout: one draft plan PR into `main`, with milestone commits and one final
fast lane. No feature gate and no setting — each change either makes an
attribute true or is not merged (§4). Cache and identity effects, stated in
the PR body: M1 and M5
change `FrozenHlsPresentation`'s fingerprint (the file's geometry, and the
new `HlsContext` fields plus the `"version"` bump), which is per-session
and invalidates nothing on disk; M2 moves fMP4 AVC masters from generation
metadata to attempt media; **M3 changes recipe identity for the hardware
families that gain `-profile:v`/`-level`, invalidating their cached SDR
transcodes, including software cells that gain an explicit level**.
Unqualified cells retain their old identity. No schema, no
settings key and no metric changes.

Rollback: each milestone reverts independently. M3's revert re-invalidates
the same cache entries a second time, which is the only rollback with a
cost, so it deploys to media1 alone for a week before the rest of the
fleet.

## 7. Open questions

1. **Which families honour the cadence-safe `-level`?** §3.3 makes no
   inheritance across cadence. If QSV or VideoToolbox rounds up, identity and
   declaration become a function of `(Encoder, height, resolved frame grid)`;
   the frozen presentation must then carry that qualified result. M3's full
   23.976/29.97/59.94/60 matrix decides it.
2. **Copy peak implementation seam.** The policy is decided in §3.4: exact
   output-fragment-index bytes and durations, with no average-as-peak fallback.
   M5 still has to identify whether the existing index can freeze that answer
   before the first master for every copy/remux/prepared-successor path; until
   it can, M5 is blocked rather than partially complete.
3. **Does `FRAME-RATE` need the same treatment?** For rolling it comes from
   the frozen source probe, which is right today because no filter changes
   the cadence — but Q4's deinterlacer would, if field output is ever
   chosen. Flag it in whichever plan lands deinterlacing; nothing here
   changes it.
4. **Does the Apple panel want the *delivered* rung instead?**
   `sessionStatus.deliveredBps` is already in the controller
   (`PlayerView.swift:3263`) and is a server fact. Once `indicatedBitrate`
   is honest the two should agree, and if they do not, the disagreement is
   itself the diagnostic. Keep the tone rule as it is until M6 reports.
5. **`master_playlist_diagnostic`'s temporary `sdr-codecs` shape** (§5.4)
   is a diagnostic value that exists for one device run. It is deleted in
   M4's own PR — confirm in review that it did not survive.

## 8. Implementation decisions

1. **Geometry is resolved once, at frozen-presentation construction.** Every
   rolling creation path already passes through that constructor, including
   cached starts, takeovers and prepared successors. One rule there prevents
   a later call site from accidentally advertising source dimensions again.
2. **AVC init inspection follows the container, not the codec name alone.**
   Encoded VOD and copy sessions publish fMP4 initialization objects; rolling
   full transcodes publish MPEG-TS and do not. Treating every `avc1` context as
   init-derived would turn a truthful static fallback into a permanent pending
   playlist on the MPEG-TS path.
3. **No advisory switch is added.** M1 and M2 make existing declarations more
   exact. M3–M6 are withheld until their required observations exist, so a
   Developer setting would expose an unqualified contract rather than useful
   readiness information.

---

## Execution log

**2026-10-01 remaining-output implementation claim:** the new owned
`codex/s10-remaining-output` branch connects real rolling-copy rename and PUT
commit observations to bounded complete full-mux accounting. ENDLIST alone,
queued bytes, body replacement, a stale attempt or an abnormal producer exit
cannot qualify a reusable output. These collectors do not themselves grant
retained-body authority or revise an issued presentation.

For normalized, exactly resolved automatic copy candidates, a distinct
version-one `CopyOutputPrepare` payload uses the existing bounded job lane,
claim, cancellation, lease and settlement owners. It does not reinterpret
`TranscodePrepare` or add a scheduler. The actual VOD driver owns a finite
private preparation incarnation, domain-separated from the ordinary key by
the reservation nonce while preserving the canonical recipe and full logical,
audio and physical-source tuple. It creates no viewer, GET or frontier demand
and never retires an ordinary partial rendition.

The full-footprint reservation charges init, metadata, media and in-flight
bytes before writes. Only successful media already present in the node's real
working-set total receives a temporary ordinary-horizon exclusion. Release
removes that exclusion, not the physical charge; retained conversion is once
only, under the existing allowance and GC. Complete verified original-epoch
output assembles privately. Exact queue settlement records historical
completion, then independent post-await source/engine/attachment validation
must succeed before registry exposure. SQL success is not cross-filesystem
atomic visibility and is never later acquisition authority.

A process-private attachment observation advances only after a real successful
foreground graph commit. Installation, the existing 250 ms preparation
watchdog and final check-plus-exposure all compare its captured value. No await
occurs under the short exposure guard; issued immutable bodies remain intact.
Only successfully settled and exposed artifacts receive an unforgeable local
prepared-origin seal. Compatible new attachments still reacquire exact bytes,
playlist, logical/audio/source facts and issued identity; no worker/public
field or durable recipe alias grants this authority. Ordinary cold playback
remains uncaptured and playable without a whole-film wait.

The actual rolling producer emits MPEG-TS `seg%05d.ts`, not always fMP4.
Metadata collection therefore supports self-initializing TS without inventing
an init or codec fact. Complete measurement requires segment zero, media
sequence zero (or its standard absent default), every committed media member
exactly once and no omitted known tail. fMP4 requires its actual MAP/init;
TS with a MAP or ambiguous mixed container remains unknown. An ENDLIST seek
suffix cannot qualify a title cost. These guards never reject playback or
promote collector numbers to retained-body authority.

Twelve new focused IDs have passed individually once so far; failed-only
retries and precursor source attribution remain in the development receipt.
Current-base composition, normal hook/compiler and independent review are
still required. **Still open:** manual-copy preparation and its original
first-publication acceptance, rolling/PUT retained-consumer qualification,
arbitrary source/audio/corpus coverage, fetched-wire equality and physical
device/fleet acceptance. This implementation claim is not complete M5.

**2026-10-01 durable completed-output continuation:** PR #680 landed as
`35275de3d86c48bcb6787033911ba5e68626bfa7`; its retained-artifact implementation
is the starting point, not repeated work. The next owned branch adds a private
versioned, bounded atomic completion manifest and lazy exact byte/provenance
validation to the real typed Restore consumer. Startup examines only bounded
metadata; it does not hash a film or create a second preparation scheduler.
Original execution nonce, epoch, process-salted recipe, playlist and init stay
immutable. An explicit resolved logical delivery tuple is checked against the
current source/request separately; old bodies never authorize a new producer
or a post-restart repair. Restart-loaded artifacts do not become candidate
proposal cost authority. Absent, invalid, timed-out or incompatible provenance
is unavailable proof: issued Restore refuses before replacing a session,
while ordinary uncaptured playback remains available and captured None remains
None. Registry reservations and init/media leases retain existing bounded GC.
The issued identity seals the ordered actual member hashes/lengths, logical
tuple, original production origin and artifact UUID; Restore compares against
the caller's independently retained seal, never self-declared manifest facts.
Only the verified normal trailer completes the original Sink epoch. Retiring
an all-done child does not revoke its published bytes; mixed-epoch or repeated
publication still poisons measurement. Seven new focused IDs have individually
passed once (four before seal hardening, plus the repaired real consumer and
two new seal/trailer negative controls); earlier failures remain recorded.
Sole review45 identified that healthy rendition sharing discarded the incoming
logical tuple. The repair preserves that tuple outside the moved Recipe and
checks it during lazy reacquisition and final attachment; ordinary differing
requests receive no borrowed proof rather than losing playback. The shared
process-salted production key remains unchanged. One additional real-consumer
regression checks same-key reuse, exact-tuple success, differing-tuple issued
refusal/incumbent preservation and ordinary uncaptured playback.
The new branch requires its own focused once-per-PR receipts and independent
review; no earlier unit pass is claimed as its evidence. Rolling-copy/PUT,
arbitrary cold-copy preparation and physical/corpus acceptance remain open.

**2026-10-01 retained VOD implementation boundary:** the candidate now retains
complete successful full-mux output under generation-distinct private hardlink
names, including init and tail. A session captures its receipt once; earlier
`None` stays `None`. Init/media GETs and streamed bodies hold the artifact,
not mutable recipe paths. Missing names may be repaired only from exact
source/execution/init identity and byte digests; a conflicting repair refuses
that artifact without poisoning an ordinary producer or rebinding an old
master. A new presentation may use a separately completed receipt.

Retention has one pre-clone assembly reservation, at most 64 artifacts,
8,192 entry metadata records and the reducer's 131,072 examined-window cap.
An OS file lease owns the private namespace. Bounded orphan/GC batches keep
unknown or failed cleanup charged; unowned, symlinked or unresolved namespaces
make measurement unavailable, not ordinary playback unavailable. No second
scheduler, payload copy, whole-title cold wait or startup media scan is added.
The private lease coordinates cooperating builds; it does not establish that
an older daemon respects a shared-cache rollout or certify deployed images.
Fresh bounded node advertisements negotiate receipt metadata in both
directions; old/unknown peers receive the old strict envelope. Durable restore
requires the exact issued artifact; legacy absence cannot acquire later facts.

Actual candidate cost is a private retained proof, bound to accepted full
candidate/digest, source version, selected audio, grade, actual recipe and
complete output incarnation. Reader and dispatch reacquire this identity.
Planned ladder budgets and `complete_cache` alone are not measured cost. A
bounded optional public HTTP sidecar carries advisory complete-full-mux RFC
cost provenance; core candidate identity and strict worker wire stay unchanged.
Its public response/client integration belongs to the coordinated A05
continuation. This implementation candidate still needs its remaining focused
consumer/compatibility checks and independent review. Rolling-copy/PUT,
persisted arbitrary cold-copy preparation and original fleet/device/corpus
acceptance remain open; this is not complete M5 qualification.

**2026-10-01 M3 continuation:** the earlier M3–M6 evidence-only classification
did not establish the encoder qualification code. This continuation owns
bounded node-local profile/level/cadence experiments and frozen SDR identity,
not SDR master emission (M4), bandwidth measurements (M5), or device/fleet
acceptance. The selected node FFmpeg must complete a real encode whose SPS,
`avcC` and every fMP4 sample duration agree. A proposed table cell is not a
codec identity; untested/refused cells retain the incumbent arguments.

The local matrix has a 30-second budget, with a three-second child deadline
and bounded cleanup, 8 MiB encoded-output and 1 MiB trace ceilings. It covers
the five proposed rung heights and four named cadences, with both 852- and
854-wide 480p experiments because the shipping even-rounded raster is 852.
Family, exact raster, rational cadence, target bitrate, effective rate-control
value and forced-IDR mode must match a completed experiment. Non-matching
cells, including rates above 60 fps, do not inherit another cell's evidence.
The final VOD fps grid is bound before its recipe and presentation freeze.
Legacy-rung qualification is restricted to `PreserveAspectEven` contracts;
the newer upright/square candidate route retains its separately explicit
profile/level and full candidate/recipe identity instead of inheriting this
matrix's flags or triplet.

Qualified software also gains a level flag: its recipe identity must change.
The older software-cache-unchanged claim applied only to the profile flag and
does not cover this implementation. Unqualified plans keep their old recipe
digest and encoder arguments. Local Homebrew FFmpeg 9 development observations
are not shipped FFmpeg 8 or fleet qualification evidence.

**2026-10-01 manual-copy reachability continuation:** the existing worker lane
also accepts a closed version-two server-resolved manual Copy intent, without
inventing candidate context. Source metadata and the full resolved audio,
offset, delivery, grade, video and engine intent are bound together. A later
compatible new attachment can acquire only the actually settled, locally
minted private artifact after physical/source/logical/engine revalidation;
persisted job success and restart manifests cannot mint this capability.
The initial uncaptured attachment remains uncaptured. This does not make
unknown full-tail facts available before preparation completes, and does not
implement cold encoded preparation or waive original corpus/device evidence.

**2026-10-02 encoded preparation continuation (in progress):** a distinct closed
version-one `EncodedOutputPrepare` intent uses the existing node-affine
preparation lane, not speculative `TranscodePrepare` or live-wait admission.
The actual foreground resolver supplies selected audio delivery and offset,
subtitle/body digest, output grade, geometry, executable and engine identity.
The worker recomputes the full intent against held current source bytes before
using the existing Background resource bundle and finite VOD preparation
reservation. Normal complete output is privately retained before exact job
settlement; only successful settlement plus post-await physical/logical/owner
revalidation exposes the process-private origin to a compatible new attachment.
Historical SQL success alone grants no attachment or artifact authority.

The new real consumer case has passed once with selected EAC3-to-AAC delivery
and a 250ms offset, without invented candidate context. Production migrations
SQLite93/Hiqlite69 follow the immutable independently owned recovery92/68
parent; a new actual SQLite upgrade case passes once. This is not yet a frozen
or independently reviewed implementation. Rolling-copy/PUT
complete observations still require actual retained consumer authority before
their rates can become wire facts. Exact first-master facts for an unseen tail
without a whole-film foreground wait, and original fleet/device/corpus
qualification, remain explicit acceptance boundaries rather than guessed costs.

**2026-10-03 private union canonical-carrier repair (source prepared, not
qualified):** compiler inspection of the historical `814888d4` + `34252101`
union exposed that the stored automatic preparation intent retained an id and
digest but could not recreate the accepted client caps, row and atomic planning
binding. The original manual encoded once-pass above deliberately had no
candidate context; it is not evidence for automatic reconstruction.

The two existing private preparation payloads now optionally retain the existing
strict `CandidateCatalogContext` at enqueue. Absent carriers are omitted during
serialization, preserving legacy/manual payload bytes and dedupe identity. The
entire payload still has the existing 16 KiB bound; canonical caps/count checks,
closed intent checks and worker unknown-field refusal are not relaxed. No
worker/public reorder field, schema migration, setting or scheduler is added.
The effective reorder choice stays bound to the same atomic planning snapshot,
planning binding and VOD recipe digest. The private response-cost sidecar is
not selected-candidate authority and is deliberately absent on internal jobs.

Claimed automatic work reconstructs the exact current authority through the
shared restore/catalog chain before opening a producer, then checks the stored
source, audio, route, geometry, grade, owner and execution intent. An older
automatic task without original canonical evidence cannot manufacture it from
a digest. Its existing fence stops that single unverifiable task; a genuine
fresh foreground enqueue is the existing replan path. Old records remain
parseable/listable/cancellable, manual work is unchanged, and backend failure
continues through the retry lifecycle. Older strict readers cannot accept
carrier-containing automatic payloads: mixed-reader eligibility is **not
claimed**, and this source record is not deployment acceptance.

Selected reconstruction preserves availability errors from the actual catalog
computation: failed source opens, executable/runtime capture, encoder/probe
selection and unavailable recipe identity are retryable, not a missing-row
permanent stop. The claimed worker's actual Store row lookup distinguishes
missing/changed source metadata (fenced `source_changed`) from a Store error
(retry). Library/source-open unavailability does not prove disappearance.

**2026-10-03 inherited ingress-deadline amendment (source only):** worker Start
passes its original early-handler deadline into canonical restore, before the
startup-budget scope is installed. Takeover passes its existing deadline while
retaining the outer timeout. Restore clamps that remaining caller allowance to
the existing 2-second catalog cap rather than starting an independent new
allowance. The compatibility/test wrapper retains the existing bounded entry;
the same still-unexecuted combined metadata control covers an expired explicit
entry and current-authority compatibility restoration. Authentication,
ownership, strict wire, restart drain and startup policy are unchanged.

The `/decision` measured-cost projection acquires an actual source/settings
snapshot matching the accepted local row's binding, within one existing 100 ms
advisory deadline. A mismatch removes only the advisory cost, not the catalog,
manual choice or ordinary playback. The catalog and restore checks preserve
the newer signed composition offsets, frame grids, quality and selected audio.

The existing new combined reorder/metadata control is extended but remains
unexecuted. One distinct real automatic canonical-carrier enqueue → claim →
completed output → compatible new attachment regression is added in
[encoded_preparation.rs](../../crates/plurxd/src/vod/tests/encoded_preparation.rs)
and also remains unexecuted. Three typed-recovery/decision controls have changed
source semantics and their earlier successes remain historical, not current
qualification. The original twenty-error compiler raw and subsequent controller
cleanup failure remain retained; no new compiler success, test success,
current-parent qualification, corpus/device observation or M5 completion is
claimed by this preparation. Current-parent integration and exact-tree checks
must precede the first observations of these new controls.

Executing sessions append one row per logical milestone in the single plan PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

**2026-10-02 rolling-copy/PUT retained-consumer continuation:** actual successful
copyseg and lane-zero PUT commits optionally hardlink their complete output
members under the existing retained namespace OS lease. Existing configured
allowance is reserved before linking; refusal leaves ordinary playback and
scratch accounting unchanged. A collection reserves at most the remaining
allowance, with the existing 64-artifact limit, 8,192-object GET-inventory
limit (including the playlist), and a finite deadline of four source durations
plus 300 seconds, capped at 24 hours. The same collector releases charges only
after exact owned unlink. Pending collection and body owners retain their
leases; no second registry, scheduler, or payload buffer is introduced.

**Review59 correction:** optional member capture synchronously reserves and
charges its single owned operation, then returns without awaiting hardlink I/O
inside Copy publication or the PUT commit mutex. A busy or expired optional
owner refuses only proof collection; completion alone may join that owner
within its existing bounded wait. Proof-enabled initial launches use the exact
captured executable path, not a second resolution of its configured symlink.
An actual verified-GET integrity failure permanently refuses that exact
artifact's acquisition entry while preserving issued body owners and charged
collector cleanup. Attachment samples irreversible refusal after source
verification yields, both before and after registration. Exact retained lookup precedes encoder and scratch
admission; its preferred graph uses the existing read-only workload/bundle
thread policy, and a later hardware demotion never broadens compatibility.

Only the original normal successful child/reader completion, verified source
duration, full canonical zero-origin ENDLIST and exact committed inventory can
mint this process-private artifact. Source bytes are inherited through the
actual producer descriptor; full resolved source/audio/offset/grade/route/argv
and executable/engine facts bind its lookup. Retry refuses predecessor proof.
On non-Linux Unix, two source demuxers remain unqualified because `/dev/fd`
shares offsets; ordinary arguments and playback remain available.

A compatible NEW local attachment reacquires actual retained bytes and current
source/engine facts before sealing measured average/contiguous-window peak in
its frozen presentation. Existing cached GET snapshot validation and body
leases authenticate the actual playlist/media bytes. No old attachment is
rebound, and no restart manifest, queue result, telemetry row, or remote field
creates rolling acquisition authority. The registry's memory-only GET
inventory is not a serialized cache-health or durable recovery proof.

The new real-copy consumer and refusal/source-replacement controls each passed
once; the original inventory control remains source-bound. Independent review,
current-source compiler/static/hook checks and the effort gate still precede
landing. This is bounded implementation, not original S10 completion:
unseen-tail exact first-publication without whole-film foreground waiting,
fetched-wire corpus equality, per-segment burst acceptance, named-device codec
compatibility and fleet qualification remain open.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-10-02 | gpt-6.1-sol | agent:/root/k06_pr725_adversarial_sol61 | M5 public Create and fetched-wire development control | pending independent review | One genuinely new ignored method passed once on exact frozen `574e88dea` /tree `8bd154b8` (parent effort `102669d27`), under the admitted Darwin FFmpeg9.0.1 watchdog. Actual public HTTP Create and GETs, exact retained source/audio tuple, independently calculated mux rates, immutable first masters, public DELETE and listener reuse asserted; external terminal zero and owned-session absence proved. One input/one media segment is not shipping Linux8.1.3, variable-window/corpus, unseen-tail, whole-film, native/device or fleet qualification. Current composition/compiler/static/gate and a different independent reviewer remain required; no successful replay. |
| 2026-10-02 | gpt-6.1-sol | agent:/root/p02_663_resume_sol61 | M5 rolling/PUT retained consumers | [#706](http://192.168.4.7:3000/noirr/plurx/pulls/706) | Optional successful commit hardlinks share existing namespace/count/allowance/cleanup; original verified completion and exact process-private source/recipe/engine binding precede a compatible NEW attachment. Five original focused successes retain historical source attribution; five new review59 controls passed once, including real integrity/producer-admission and post-await refusal controls. Same sole review59 disposition remains required. No restart/telemetry authority, old-owner rebinding, unseen-tail or physical qualification claim. |
| 2026-10-01 | gpt-6.1-sol | agent:/root/p02_663_resume_sol61 | M3 claim | pending draft | Own clone `plurx-s10-m3-sol61`, branch `codex/s10-m3-encoder-qualification`, original actual effort `903201a24`; pinned Rust 1.97.1 baseline passed before Rust edits. M1/M2, output-codec and audio contracts retained; no M4/M5 or fleet/device acceptance claim. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | Claim | [#419](http://192.168.4.7:3000/noirr/plurx/pulls/419) | Claimed `plan/S-10` from `665b8b5c`; M1–M2 are locally implementable, while M3–M6 remain evidence-gated. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M1 | [#419](http://192.168.4.7:3000/noirr/plurx/pulls/419) | Rolling frozen presentations use `output_size`; three focused rolling-geometry tests and the copy-session guard passed. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M2 | [#419](http://192.168.4.7:3000/noirr/plurx/pulls/419) | Bounded `avcC` parsing, fMP4 normalization, MPEG-TS bypass and attempt-media classification passed focused tests. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M3–M6 | [#419](http://192.168.4.7:3000/noirr/plurx/pulls/419) | needs: fleet encoder/SPS qualification, named-device SDR `CODECS` re-qualification, measured corpus peak/average/overhead, and Apple-panel before/after observations. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | Sole review [#3318](http://192.168.4.7:3000/noirr/plurx/pulls/419#issuecomment-3318) | `9ca2cb23` | Resolved all four findings: structural selected-track `stsd`/`avcC` identity with decoy/duplicate/typed-refusal tests; coherent encoded-VOD no-upscale/unprobed geometry; cadence-safe proposed levels with 23.976/29.97/59.94/60 evidence; copy/remux/prepared-successor fragment-index peak contract. Pinned 1.97.1 focused AVC (4), fMP4 AVC (2), MPEG-TS (1), VOD geometry (2), affected Clippy, rustfmt, docs index and diff check green; no broad unit. |
