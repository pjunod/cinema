# Downmix qualification on real content — the fold clips, the gains were fine

**Status:** measured on real content and shipped as the stereo fold; listening
notes and device evidence open · **Measured:** 2026-10-02 on lab3 (the Forgejo
host) · **Source:** effort `ae83960b3` · **Model / session:** claude-opus-5-5 /
https://claude.ai/code/session_01CAyBrYCQ7PpAtuZwUxKfp7

Companion to [Audio resolved independently](AUDIO-RESOLVED-INDEPENDENTLY.md)
§3.4 and §5.4, and to the
[synthetic receipt](AUDIO-DOWNMIX-SYNTHETIC-QUALIFICATION.md), whose
coherent-sine and FC-only inputs this replaces with nine 90-second windows of
real film soundtracks. Window names below are neutral labels; the private
receipt maps them to library files, decoded-PCM SHA-256s and exact commands.

## Engine and method

`ffmpeg version 8.1.3-Jellyfin`, binary SHA-256 `90004301…7fbb54678837f224d3458032e38787`
— the same binary the synthetic receipt measured — run in throwaway containers
of the production image (`--network none`, library read-only). Every
candidate was rendered **straight from the source container** (a WAV
intermediate drops the AC-3/E-AC-3 downmix metadata, which is how finding 2
surfaced), both as pre-AAC float PCM and as AAC-LC 160 kb/s 48 kHz decoded
back. Metrics: `ebur128=peak=true+sample` (integrated LUFS, true peak), exact
counts of decoded samples at or above full scale and above −1 dBFS, limiter
action (share of samples cut by more than 0.1 dB, and the largest cut). The
dialogue proxy applies each matrix to the source with only FC kept and with
FC zeroed: *centre per side* relative to the source FC (target −3 dB), and
*centre over the rest* in LU. FC carries music and effects too, so it is a
proxy. The incumbent candidate D was also rendered with the production VOD
audio chain and matched D on every window.

Candidates: **D** — production before this change, `-ac 2` (swresample's
float default fold); **Pf** — the plan's §3.4 named matrix after
`aformat=sample_fmts=fltp`; **N** — the normalised fold the plan assumed
production used.

## Results

"Overs" are decoded AAC samples at or above full scale, per side, out of 4.32 M.

| Window | Source | D AAC LUFS / TP / overs | Pf AAC LUFS / TP | D−Pf LU | Centre per side D / Pf / N (dB) | Limiter at −2 dBFS (samples cut, max cut) |
|---|---|---|---|---|---|---|
| A drama dialogue | DTS-HD MA, 5.1(side) | −17.8 / −0.7 / 0 | −17.8 / −0.7 | 0.0 | −3.0 / −3.0 / −10.6 | 0.81 %, 2.2 dB |
| B action | E-AC-3, 5.1(side), stored levels 0.595/0.5 | −17.2 / −1.6 / 0 | −16.3 / −0.8 | −0.9 | −4.5 / −3.0 / −10.6 | 0.49 %, 1.3 dB |
| C music | E-AC-3, 5.1(side), stored levels 0.595/0.5 | −19.1 / −2.4 / 0 | −18.4 / −2.0 | −0.7 | −4.5 / −3.0 / −10.7 | 0 |
| D concert | AC-3, 5.1(side), stored levels 0.707/0.707 | −19.6 / −5.1 / 0 | −19.6 / −5.1 | 0.0 | −3.0 / −3.0 / −10.6 | 0 |
| D′ concert, same window | TrueHD, 7.1 | −11.7 / +2.8 / 49/22 | −11.9 / +2.8 | +0.2 | −3.0 / −3.0 / −12.9 | 14.0 %, 5.5 dB |
| E action, loudest | TrueHD, 7.1 | −11.9 / +4.8 / 621/211 | −12.1 / +4.2 | +0.2 | −3.0 / −3.0 / −12.9 | 26.6 %, 9.0 dB |
| E′ dialogue | TrueHD, 7.1 | −27.9 / −8.3 / 0 | −28.0 / −8.3 | +0.1 | −3.0 / −3.0 / −12.8 | 0 |
| F loud LFE | DTS-HD MA, 5.1(side) | −11.6 / +5.0 / 200/288 | −11.6 / +6.2 | 0.0 | −3.0 / −3.0 / −10.7 | 13.2 %, 8.9 dB |
| G dialogue and score | AAC, 5.1 (back) | −16.2 / −0.2 / 0 | −16.2 / −0.4 | 0.0 | −3.0 / −3.0 / −10.7 | 0.50 %, 3.8 dB |

Library census (299 files): 264 × 5.1(side), 60 × 7.1, 7 × 5.1, about five
six-channel files with no layout, one 5.0(side), and no 6.1, 4.0, 3.0 or 2.1.

## Findings

1. **No 7–8 dB dialogue loss.** AAC wants float, so the default fold is not
   normalised: on DTS, TrueHD, AAC and AC-3 it equals the named 5.1 matrix to
   1.9e-4 per sample, centre −3.0 dB per side. Only N loses 7.6–9.9 dB, and
   nothing selected N.
2. **AC-3/E-AC-3 apply their stored downmix levels.** ffmpeg overrides
   `clev`/`slev` with the bitstream's metadata even when they are set
   explicitly. Both E-AC-3 titles store −4.5 dB centre / −6 dB surround, so D
   put dialogue 1.5 dB below the named matrix and 1.1–1.3 LU less prominent —
   outside the plan's ±1 LU bar.
3. **Production clipped.** Unlimited folds go over full scale on five of nine
   windows; the incumbent delivered decoded AAC at or above 0 dBFS on three
   windows (up to 621 samples per side) and above −1 dBFS on five.
4. **The §3.4 string as written hard-clips on TrueHD and DTS-HD MA.** Those
   decoders produce 32-bit integer samples and `pan` mixes in its input format,
   so it saturates before any limiter (593, 21,870 and 63,481 samples on D′, F
   and E). `aformat=sample_fmts=fltp` first removes it.
5. **The synthetic −2 dBFS ceiling fails on real content:** window D′ decoded
   to +0.32 dBFS because AAC overshoots the limiter ceiling by up to 2.4 dB.
   −3 dBFS passes by 0.17 dB on one window; **−4 dBFS keeps every window at or
   below −2.47 dBFS**.

## The fold that ships

Every stereo fold: float conversion, a named matrix where the source's own
layout spelling and channel count identify one, then one look-ahead limiter at
−4 dBFS with auto-level off. Other layouts (none of the uncommon ones exist in
the library) take the float default fold plus the same limiter, never a matrix
guessed from a channel count. Non-stereo targets (7.1 → 5.1) were measured
on 2026-10-05 and now fold the same way: see the
[7.1 → 5.1 receipt](../evidence/audio-downmix-7-1-to-5-1-2026-10-05.md).

```text
5.1(side): aformat=sample_fmts=fltp,pan=stereo|FL=FL+0.707*FC+0.707*SL|FR=FR+0.707*FC+0.707*SR,alimiter=limit=0.6309573444801932:level=0:latency=1
5.1:       aformat=sample_fmts=fltp,pan=stereo|FL=FL+0.707*FC+0.707*BL|FR=FR+0.707*FC+0.707*BR,alimiter=limit=0.6309573444801932:level=0:latency=1
7.1:       aformat=sample_fmts=fltp,pan=stereo|FL=FL+0.707*FC+0.5*SL+0.5*BL|FR=FR+0.707*FC+0.5*SR+0.5*BR,alimiter=limit=0.6309573444801932:level=0:latency=1
other:     aformat=sample_fmts=fltp:channel_layouts=stereo,alimiter=limit=0.6309573444801932:level=0:latency=1
```

Loudness cost against the incumbent: 0.1–0.4 LU on ordinary scenes, 1.0–2.0 LU
on the three loudest, which the incumbent was hard-clipping. The E-AC-3 titles
become 0.7–0.8 LU louder because the named matrix replaces their stored
levels — chosen deliberately (2026-10-02, recorded for Paul's review): it meets
the ±1 LU dialogue bar and gives one fold identity per layout. Honouring the
stored levels instead means routing AC-3/E-AC-3 through the "other" row.

## Encoded-VOD join and limiter timing

The fold runs at the head of the encoded-VOD audio chain, on the two-second
decoded preroll, before the sample-lattice trim. Measured with the exact chain
on windows E and F, generation B starting six GOPs after generation A against
one continuous generation:

- **Join:** pre-AAC float PCM of generation B is bit-identical to the
  continuous output over 10 s (max difference 0.0), while the limiter is
  cutting more than 0.1 dB on 14–53 % of those samples; the preroll settles
  it completely. AAC differences of 0.35–0.56 at the join are the encoder
  restart, identical without the fold.
- **Timing:** folded against an unlimited fold, cross-correlation lag 0 with
  the limiter idle and cutting; sample counts unchanged. `latency=0` would
  instead delay audio by 239 samples (5 ms), so `latency=1` is required.
- **Pre-existing, not this change:** adjacent generations land up to 32
  samples (0.67 ms) apart on these MKV sources with or without the fold
  (+16 / −32 / 0 / −24 at 1 / 3 / 6 / 12 GOPs on window F). Likely MKV's
  millisecond timestamps meeting `aresample=async=1:first_pts=0`; unproven,
  reported to the VOD encoding owner.

## Still open

Listening notes on the three heavily limited windows (pumping against the
incumbent's clipping), and M3/M5 device-route acceptance.
