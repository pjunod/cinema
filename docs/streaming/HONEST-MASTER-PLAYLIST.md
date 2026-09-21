# Honest master playlist — say what this session delivers, not what the file is

**Status:** ready for review · **Executes:** Q7 / F-stream-14 / A11 /
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

Board id S-10. Milestones are one draft PR each into `main` under the fast
lane.

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
`["hvc1", "hev1", "dvh1", "dvhe", "avc1"]`, and a new
`avc_codec_from_init(&init) -> Option<String>` walks to the `avcC` box and
reads three bytes:

```text
  avcC: configurationVersion(1) AVCProfileIndication profile_compatibility
        AVCLevelIndication ...
  CODECS := format!("avc1.{:02X}{:02X}{:02X}",
                    profile_indication, profile_compatibility, level_indication)
```

This is the same shape as `hevc_codec_from_init`
(`hls.rs`, tested at `hls.rs:27995`), so it gets the same treatment: a unit
test over a hand-built box, and the same `HlsInitInspectionError`
pending/invalid/unavailable classification on a short or malformed init.
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
2. **Derive the string from the rung.** Replace the SDR arm of
   `transcoded_hls_codecs` with a function of the forced profile and level:

   | Rung (`output_size` height) | Max luma samples/s at 60 fps | H.264 level | `CODECS` |
   |---|---|---|---|
   | 360 | 0.31 M/frame | 3.0 (`0x1E`) | `avc1.64001E` |
   | 480 | 0.41 M/frame | 3.1 (`0x1F`) | `avc1.64001F` |
   | 720 | 0.92 M/frame | 3.2 (`0x20`) | `avc1.640020` |
   | 1080 | 2.07 M/frame | 4.0 (`0x28`) | `avc1.640028` |
   | above 1080 (Original-class) | 8.29 M/frame | 5.1 (`0x33`) | `avc1.640033` |

   The levels above are the **proposed** mapping, not a measured one. They
   are derived from H.264 Annex A's MaxMbPS/MaxDpbMbs bounds at the rung's
   frame size and this ladder's bitrates; the *acceptance* is that the
   encoded bitstream's SPS `level_idc` equals the declared one on every
   enabled family (§5.3), not that the table looks right. Where a family's
   driver refuses `-level` or emits a different `level_idc`, the table
   takes the measured value for that family, and the function becomes a
   function of `(Encoder, height)` rather than of `height` alone.

The constraint-flags byte stays `0x00`: High profile with no constraint set
is what `-profile:v high` produces, and a nonzero byte would be a claim
about `constraint_set*_flag` nobody has read.

`bitrate_for_height` (`transcode.rs:27519-27526`) is the only other place
the rung's identity turns into numbers; nothing in this plan changes it.

**This changes recipe identity.** `encode_args_for`'s output is part of the
argument list a recipe hashes, so adding `-profile:v` / `-level` to the
hardware families invalidates every cached SDR transcode produced by those
families. That is correct — the bytes genuinely differ — and it must be in
the PR body, with the note that software-encoded entries are unaffected
because `-profile:v high` was already there. The rung-derived `CODECS`
string is playlist metadata and does not itself invalidate anything.

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

Copy sessions keep `file.bitrate` for `AVERAGE-BANDWIDTH` — for copied bytes
that *is* the average — and take their peak from the probe's per-stream
`max_bit_rate` when present, otherwise `file.bitrate` scaled by the same
measured factor. That is §7's open question 2, not a decision this document
takes.

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

One draft PR each, into `main`, under the fast lane (`make unit`).

### 5.1 M1 — rolling geometry from the rung

Code: §3.1, at all four rolling freeze sites plus
`refresh_frozen_presentation_from_store`. No client change.

Tests, in `transcode.rs`'s existing module:

| Test | Asserts |
|---|---|
| `a_rolling_transcode_freezes_the_rung_geometry` | 3840x2160 source at the 720 rung freezes 1280x720; aspect preserved, both sides even |
| `a_rolling_transcode_of_an_unprobed_source_freezes_no_geometry` | `width`/`height` stay `None`; master omits `RESOLUTION` |
| `a_rolling_transcode_never_upscales_its_declaration` | 640x360 source at the 1080 rung freezes 640x360 |
| `a_probe_refresh_keeps_the_frozen_rung_geometry` | `refresh_frozen_presentation_from_store` after a store row change still reports the rung's geometry |
| `a_prepared_successor_declares_its_own_rung` | prepared 480 successor to a 1080 incumbent: the successor's master says 854x480 (or the source-aspect equivalent) |

Acceptance: `cargo test -p plurxd frozen_presentation rung_geometry` green;
`make unit` green; a manual `curl` of `master.m3u8` for a rolling 720
session on a 4K fixture prints `RESOLUTION=1280x720`.

### 5.2 M2 — the AVC string from `init.mp4`, for fMP4 sessions

Code: §3.2, plus adding `avc1` to `master_requires_attempt_init`
(`transcode.rs:5630-5634`) and the fingerprint version bump from §3.5 if
M3 has not landed it. `transcoded_hls_codecs`'s SDR arm is still the
fallback in this PR; M3 replaces it.

Tests:

| Test | Asserts |
|---|---|
| `avc_codec_from_init_reads_the_avcc_triplet` | hand-built `avcC` with 0x64/0x00/0x28 -> `avc1.640028` |
| `avc_codec_from_init_refuses_a_truncated_box` | short box -> `None`, not a panic or a partial string |
| `an_fmp4_avc_session_normalises_its_codec_from_the_init` | `exact_hls_context_at` returns the init's string, not `transcoded_hls_codecs`'s |
| `an_mpegts_session_keeps_its_static_codec_string` | rolling session: no init read attempted, context unchanged |
| `an_fmp4_avc_master_is_attempt_media_not_generation_metadata` | `sealed_stable_master_contract` is `None` for an `avc1` context |

Acceptance: `cargo test -p plurxd avc_codec_from_init exact_hls_context`
green; `make unit` green; on a dev server, an encoded-VOD session's
`master.m3u8` fetched twice returns the same string and a
`grep session_init_object` of the logs shows exactly one init read per
session.

### 5.3 M3 — forced profile/level on hardware, and a rung-derived string

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
| `every_sdr_family_forces_a_profile` | `encode_args_for(Sdr, ..)` contains `-profile:v high` for all five families |
| `the_declared_level_follows_the_rung` | 360/480/720/1080/2160 map to the §3.3 table |
| `a_family_that_refused_the_probe_keeps_its_old_arguments` | caps with the new verdict false -> argv identical to `0f02b7ea`'s |
| `the_recipe_hash_changes_for_a_family_that_gained_the_flags` | two hashes differ; a software recipe's hash does not |

Bitstream acceptance — the level in the argv must equal the level in the
SPS. Per enabled family on a node that has it, for each rung:

```bash
# one 20 s rolling session per rung, then read the SPS the encoder emitted
ffprobe -v error -select_streams v:0 \
  -show_entries stream=profile,level,width,height \
  -of default=nw=1 seg00003.ts
```

`level` must equal the declared `0xLL` as a decimal (e.g. `40` for `0x28`),
`profile` must be `High`, and `width`x`height` must equal the master's
`RESOLUTION`. A mismatch on any family replaces that family's row in the
§3.3 table with the measured value before the PR merges.

Acceptance: `cargo test -p plurx-core encode_args_for level` and
`cargo test -p plurxd transcoded_hls_codecs` green; `make unit` green; the
ffprobe table above filled for every family the fleet actually selects (see
the GPT prompt in §6).

### 5.4 M4 — re-qualify the SDR ruling on the named devices, then print `CODECS`

No code lands until the device run reports. The run uses
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

Acceptance: declared `BANDWIDTH >= measured peak` for every segment of
every fixture and rung; declared `AVERAGE-BANDWIDTH` within +/-5 % of the
measured average and never below it; `cargo test -p plurxd
master_bandwidth` green; `make unit` green; the table in the PR body.

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
   transcode session of a 4K SDR fixture, wait 30 s, find the session's
   scratch directory under the transcode root, and run
   `ffprobe -v error -select_streams v:0 -show_entries
    stream=profile,level,width,height -of default=nw=1 <seg00003.ts>`.
   Report profile, level, width, height, and the node's encoder.
3. Repeat step 2 at the 360, 480 and 1080 rungs.
4. Also fetch that session's master playlist
   (`curl -s http://<host>:32400/hls/<session>/master.m3u8`) and paste the
   `#EXT-X-STREAM-INF` line beside each ffprobe row.
Report one table per node: rung, encoder, declared RESOLUTION, actual
width x height, declared CODECS, actual profile/level.
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

Rollout: one draft PR per milestone into `main`, fast lane. No feature
gate and no setting — each change either makes an attribute true or is not
merged (§4). Cache and identity effects, stated in each PR body: M1 and M5
change `FrozenHlsPresentation`'s fingerprint (the file's geometry, and the
new `HlsContext` fields plus the `"version"` bump), which is per-session
and invalidates nothing on disk; M2 moves fMP4 AVC masters from generation
metadata to attempt media; **M3 changes recipe identity for the hardware
families that gain `-profile:v`/`-level`, invalidating their cached SDR
transcodes** — software-encoded entries are unaffected. No schema, no
settings key and no metric changes.

Rollback: each milestone reverts independently. M3's revert re-invalidates
the same cache entries a second time, which is the only rollback with a
cost, so it deploys to media1 alone for a week before the rest of the
fleet.

## 7. Open questions

1. **Is a per-family `CODECS` function needed, or does `-level` hold?**
   §3.3 assumes every enabled family honours `-level` and emits the
   `level_idc` it was given. If QSV or VideoToolbox rounds up, the string
   becomes a function of `(Encoder, height)` and the frozen presentation
   must then carry the encoder, which it does not today. M3's bitstream
   acceptance decides this; if it goes the wrong way, M3 is two PRs.
2. **Copy sessions' peak.** §3.4 leaves copy on `file.bitrate` for both
   attributes. The probe's per-stream `max_bit_rate` is often absent in
   MKV. Options: leave copy alone (honest-ish, understates burst), scale by
   the measured container factor, or compute a real peak at scan time from
   the fragment index the store already builds. The third is the only
   correct one and it is the most work; decide after M5's numbers show how
   badly a remux's peak exceeds its average.
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
