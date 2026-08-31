# M5a — the container-truth check that has to run on real media

**Status:** waiting on a node and a disc remux · **Executes:** fable's §7
verification protocol for the Profile 7 → 8.1 conversion · **Written:**
2026-08-31 · **Blocks:** nothing in CI; it is the last thing between PR #716
and confidence

Everything in PR #716 is green on synthetic fixtures. Those fixtures carry a
real captured Profile 7 RPU injected into a real ffmpeg pipe's output — which
proves the rewrite, the framing and the wiring, and proves nothing at all
about a real disc remux's timeline. This is that check.

---

## 1. Why it exists, in one paragraph

The design this milestone opened with put the RPU rewrite **between two
ffmpegs**, on a raw Annex B elementary stream. It was withdrawn because
ffmpeg's raw HEVC demuxer emits every packet with no timestamps and the muxer
then fabricates a decode-order grid: `pts == dts` for every sample,
presentation reordering erased. Measured on an ordinary three-B-frame encode,
~410 display-order inversions in 819 frames, on every GOP.

**The first measurement of that pipe looked fine**, because the harness read
through ffmpeg's best-effort-timestamp layer, which reconstructs a plausible
answer from a stream that has none. That is the mistake this document exists
to avoid repeating: `scripts/verify-dv-timeline` reads container timestamps
only, never decoded frames, never best-effort values.

---

## 2. What to run

On **nuc4**, with a real Profile 7 disc remux — `Nosferatu (2024)
Remux-2160p.mkv` is the file the RPU fixture was captured from, so it is the
obvious choice.

```bash
# 1. Capture what a converting session actually serves. Play the title on a
#    client that takes profile 8 and not 7 (Safari or Apple TV), then take the
#    rendition's init plus its segments in playlist order:
cat init.mp4 seg-*.m4s > /tmp/served.mp4

# 2. The check. Container timestamps only, no decode.
python3 scripts/verify-dv-timeline "/path/to/Nosferatu (2024) Remux-2160p.mkv" /tmp/served.mp4
```

Exit 0 is a pass. The script prints three lines before its verdict, and each
is worth reading even on a pass:

| Line | What a bad value means |
|---|---|
| `inversions: N total, M beyond the head window, head ok/DISPLACED` | Any `M > 0`, or `DISPLACED`, is the withdrawn design's failure. It must be `0` and `ok`. |
| `skew: median ±X ms, tail spread Y ms, frame Z ms` | Median must be within 5 ms and spread within 1.5 frames. Audio *lead* (negative) is the perceptually worse direction — ITU-R BT.1359-1 puts detectability at ~45 ms for lead against ~125 ms for lag. |
| `ctts (pts-dts) mismatches: N/total` | Must be 0. This is the direct statement that presentation reordering survived. |

**The head window forgives inversions in the first 32 display positions**,
because an open-GOP seek really does emit leading pictures that reorder there.
`head_ok` is what stops that forgiveness being a blanket amnesty: each of the
first eight output packets must still land near the front. A stream whose
whole timeline was rebuilt in decode order would otherwise pass the head
window by having its damage start inside it.

---

## 3. Run it seeked, not only from zero

A session that starts at zero was correct even under the withdrawn design.
plurx's VOD producer respawns at plan boundaries mid-film, so **seeks are the
normal case**, not the exception. Repeat the capture from at least two
mid-film positions — a plan boundary and something a few minutes past one —
and run the script against each.

---

## 4. The other three things worth checking while the media is in hand

These are not the timeline check; they are the facts no synthetic fixture on
this branch can establish.

1. **What the muxer wrote.** `ffprobe -show_streams` the served init and read
   its `side_data_list`. Expect `dv_profile: 8`, `el_present_flag: 0`,
   `dv_bl_signal_compatibility_id` matching the source's, and the box spelled
   `dvvC` rather than `dvcC`. The code takes the *replace* branch of
   `set_dolby_vision_record` if ffmpeg copied the source's Profile 7 record
   through and the *append* branch if it wrote none — both are implemented,
   and **no test on this branch exercises the replace branch on a real muxer
   init**, because the fixtures inject RPUs into an already-muxed stream. This
   tells you which branch production actually takes.

2. **MEL or FEL.** The daemon logs it once per session and once per index
   pass — grep for `converting Dolby Vision to Profile 8.1` and `indexed a
   converted stream`. It matters to the viewer: MEL carries no picture detail
   of its own so dropping it is lossless, while FEL carries real residual
   detail and dropping it is not. Nosferatu's captured RPU reads as **FEL**,
   so expect the "its residual detail is lost" clause.

3. **Throughput.** M5a's acceptance asks for ≥ 1.5× realtime on nuc4 for a 4K
   remux. The rewrite is per-NAL over samples the muxer already produced, so
   it should be nearly free relative to the I/O — but "should be" is not a
   measurement.

---

## 5. What a failure means

If the timeline check fails, **do not tune anything**. The post-mux design's
whole claim is that timestamps are copied and never reconstructed, so there is
no constant to fit and nothing to anchor. A failure means something is wrong
with that claim, and the useful next step is the script's own output — which
of the three lines failed, and whether it failed only on seeked runs.

This project has already had one finding retracted for being measured against
a control that never runs in production. A number that disagrees with the
design is a reason to re-examine the design, not to correct the number.
