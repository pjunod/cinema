# 7.1 → 5.1 downmix — the incumbent fold clips; every fold is now limited

**Status:** measured on the production engine and shipped as the non-stereo
fold; listening notes and device evidence open · **Measured:** 2026-10-05 on
lab3 · **Source:** main `70b72f3be` plus the F-M2 harness · **Model /
session:** claude-opus-5-5 /
https://claude.ai/code/session_014JowGeyJXJkRPwfiMtyiqL

Measurement 1 of the quality-and-streaming follow-ups handoff (§6.1, F-M2).
Companion to [Audio resolved independently](../streaming/AUDIO-RESOLVED-INDEPENDENTLY.md)
§5.4 and to the stereo receipts
([synthetic](../streaming/AUDIO-DOWNMIX-SYNTHETIC-QUALIFICATION.md),
[real content](../streaming/AUDIO-DOWNMIX-REAL-CONTENT-QUALIFICATION.md)).
Until this run a non-stereo target kept FFmpeg's plain `-ac 6` with no
limiter, marked `requires_layout_measurement`. Window and title names below
are neutral labels; the private map to library files stayed on the host.

## Answer

The incumbent 7.1 → 5.1 fold clips on real content. swresample's default
matrix adds each side surround into its back surround at −3 dB
(`BL = BL + 0.707·SL`) and, because the encoders take float, does not
normalise. A whole-title scan of one DTS-HD MA 7.1 film (T37) put the folded
back surrounds at **+2.26 / +4.51 dBFS, with 70 / 25 audio frames at or above
full scale**, while its own channels never exceed −0.1 dBFS. The decoded
E-AC-3, AC-3 and AAC deliveries of the same scenes all carried those overs.

So every non-stereo fold now ends in the stereo folds' limiter:

```text
aformat=sample_fmts=fltp:channel_layouts=5.1,alimiter=limit=0.6309573444801932:level=0:latency=1
```

`DownmixMatrix::LimitedDefaultTo { source_channels, target_channels }` emits
that for any target other than stereo, with the target count's FFmpeg default
layout. The gains are the incumbent's: with the limiter idle the float fold
matches the incumbent sample for sample (difference 0.0 on every idle channel
of every window). `requires_layout_measurement` is retired; a stored snapshot
that still carries it reads as the unfiltered `-ac` fold it described.

**Why not a `pan` matrix.** A fixed `pan=5.1|…|BL=BL+0.707*SL|BR=BR+0.707*SR`
was measured first and reproduces the incumbent on a true 7.1 decode
(difference ≤ 7.6e-5, the 0.707 rounding). But the DTS-HD MA title T52,
decoded from an input seek at 4726 s or 4740 s, comes out of the production
decoder as its **5.1(side) core** (`6,5.1(side)`), while a seek at 4730 s
decodes the full 7.1. A fixed matrix would then drop the side surrounds by
3 dB; swresample folds the layout the decoder actually emits (side surrounds
straight into the back pair at unity). The layout-following fold is the one
that is right in both cases.

## Engine and isolation

The production image's FFmpeg (`8.1.3-Jellyfin`, SHA-256
`90004301…7fbb54678837f224d3458032e38787`, ffprobe `2f270d6f…`), the same
binary both stereo receipts measured. Media work ran in a throwaway container
of the running production image: network none, all capabilities dropped,
no-new-privileges, the SSH user's UID, 2 CPUs, CPU shares 128, 2 GiB with no
swap, 128 PIDs, the library mounted read-only. Python orchestrated on the
host; every command had a 120 s deadline (one hour for a whole-title scan).
The node had no transcode or live-TV session before the run
([environment](audio-downmix-7-1-to-5-1-2026-10-05/environment.txt)).

## Method

`scripts/audio-downmix-qualification/layout.py` is the stereo collectors'
harness with the target layout as a parameter. It reuses the executed
`collector.py` helpers (unchanged; their hashes stay pinned) and renders every
output with the production encode argv for the target — `push_audio_delivery_args`:
`-c:a C -ac 6 -channel_layout:a 5.1 -b:a R -ar 48000` with E-AC-3 640k, AC-3
640k and AAC 320k, plus pre-encode float PCM. Each output is decoded and
measured per channel (sample peak, samples at or above full scale, RMS,
integrated LUFS); a candidate chain is also compared sample by sample with the
incumbent's PCM (largest difference, share of samples the limiter lowered by
more than 0.1 dB, largest cut).

- **Synthetic:** the stereo receipt's two signals at 7.1 — FC-only pink noise
  at −20 dBFS RMS and the phase-coherent −3 dBFS sine on all eight channels.
- **Real windows:** 25 × 60 s, each copied once from the library as its own
  bitstream (no WAV intermediate) and decoded by every render: R01–R16 (eight
  TrueHD 7.1 titles at 60 % and 85 %), S01–S06 (one E-AC-3 7.1 and two
  DTS-HD MA 7.1 titles), H01–H03 (the hottest moments the scans found).
- **Whole-title scans:** T37 and T52 (both DTS-HD MA 7.1) streamed through
  the exact production conversion into `astats` with a per-frame reset; every
  audio frame's per-channel peak is reduced to maxima and counts.

```bash
S09_CONTAINER=owned-container python3 owned-root/layout.py /absolute/owned-root \
  --source-layout 7.1 --target-layout 5.1 \
  --candidate "limited_default_to=aformat=sample_fmts=fltp:channel_layouts=5.1,alimiter=limit=0.6309573444801932:level=0:latency=1" \
  --real LABEL=PATH@START+60 ... --scan LABEL=PATH ...
python3 -m unittest tests.operations.test_audio_downmix_qualification
```

[Results](audio-downmix-7-1-to-5-1-2026-10-05/results.json) and
[executed argv](audio-downmix-7-1-to-5-1-2026-10-05/commands.json) are
retained; library paths appear only as `<real-source>` and a SHA-256.

## Results

Whole titles, incumbent fold, pre-encode float PCM (peak dBFS / frames at or
above 0 dBFS):

| Title | Frames | FL | FR | FC | LFE | BL (folded) | BR (folded) |
|---|---:|---|---|---|---|---|---|
| T37 | 62,200 | −0.10 / 0 | −0.10 / 0 | −0.10 / 0 | −0.14 / 0 | **+2.26 / 70** | **+4.51 / 25** |
| T52 | 91,694 | −1.25 / 0 | −1.05 / 0 | −1.04 / 0 | −1.88 / 0 | −0.25 / 0 | −1.15 / 0 |

Hot windows and the worst case, folded pair (peak dBFS / samples at or above
full scale, summed over BL+BR), incumbent → limited:

| Case | PCM | E-AC-3 640k | AC-3 640k | AAC 320k | Limiter: samples cut, largest cut |
|---|---|---|---|---|---|
| Synthetic coherent sine | +1.65 / 432,000 → −4.00 / 0 | +1.65 / 431,802 → −4.00 / 0 | +1.65 / 431,802 → −4.00 / 0 | +4.28 / 1,794 → −0.11 / 0 | 95.8 %, 5.65 dB |
| H01 (T37) | +4.51 / 2,049 → −4.00 / 0 | +4.51 / 2,072 → −3.48 / 0 | +4.43 / 2,045 → −3.66 / 0 | +4.00 / 196 → −1.99 / 0 | 6.9 %, 8.51 dB |
| H02 (T37) | +2.74 / 4,258 → −4.00 / 0 | +2.74 / 4,210 → −3.64 / 0 | +2.76 / 4,203 → −3.68 / 0 | +2.68 / 352 → −2.49 / 0 | 12.7 %, 6.74 dB |
| H03 (T52) | −0.25 / 0 → −4.00 / 0 | −0.33 / 0 → −3.82 / 0 | −0.25 / 0 → −3.83 / 0 | −0.15 / 0 → −2.89 / 0 | 32.8 %, 3.75 dB |

Across all 25 real windows the folded pair reached full scale only in H01 and
H02 with the incumbent, and in none with the limited fold, in PCM or any
decoded codec. The FC-only synthetic passes the centre through untouched
(−6.49 dBFS peak in and out). The limiter acted in 15 of the 25 windows; its
loudness cost per channel is 0.0–0.6 LU on ordinary windows and up to 2.2 LU
on the loudest (H02), the same order as the stereo fold's 1.0–2.0 LU. It is
one linked limiter, so in loud scenes it also lowers the front channels.

## Findings beyond the fold

1. **AAC 5.1 at 320 kb/s overshoots on its own.** On S03 and S04 (DTS-HD MA,
   channels at about −2 dBFS) the decoded AAC centre reached +1.53 and
   +2.10 dBFS with the incumbent (18 and 44 samples) and still +0.09 and
   +3.00 dBFS after the −4 dBFS limiter (2 and 5 samples). These are
   pass-through channels the fold does not touch, so a 5.1 source encoded to
   AAC 5.1 has the same overshoot. E-AC-3 and AC-3 at 640 kb/s stayed within
   about 1 dB of their input. Not addressed here.
2. **DTS-HD MA can decode as its core after a seek.** The production decoder
   produced 6 channels, 5.1(side), from T52 at input seeks of 4726 s and
   4740 s, and 8 channels, 7.1, at 4730 s. A rolling session that starts
   there would deliver the lossy core's layout. It also affects the stereo
   fold: `lo_ro_7_1` is chosen from the stored `7.1` spelling and gives the
   side surrounds 0.5, so a core decode would put them 3 dB below the
   `lo_ro_5_1` gain they should get. Not addressed here.

## Still open

Listening notes on the windows the limiter cuts hardest (H01, H02, S03), and
M3/M5 device-route evidence for 5.1 deliveries.
