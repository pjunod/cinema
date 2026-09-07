# M5a — the container-truth check that has to run on real media

**Status:** runnable, never yet run · **Executes:** fable's §7 verification
protocol for the Profile 7 → 8.1 conversion · **Written:** 2026-08-31 ·
**Revised:** 2026-09-03 · **Blocks:** nothing in CI; it is the last thing
between PR #716 and confidence

Everything in PR #716 is green on synthetic fixtures. Those fixtures carry a
real captured Profile 7 RPU injected into a real ffmpeg pipe's output — which
proves the rewrite, the framing and the wiring, and proves nothing at all
about a real disc remux's timeline. This is that check.

**Read §2 before §3.** The first attempt to run this document failed on
prerequisites, not on the measurement, and the corrections are in §2. The
file name says nuc4; the check runs on whichever node owns the session, which
is usually not nuc4. See §2.1.

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

## 2. Prerequisites, each of which has already blocked a run

Four things must be true before there is anything to measure. On
2026-09-02 none of them were, and the check reported "no converting
rendition" four times before the cause was found.

### 2.1 You must be on the node that owns the session

Renditions are node-local. A session is owned by one node and its
`/srv/plurx/cache/renditions/` entry exists only there — running the check
anywhere else reports an empty cache and tells you nothing.

| Node | Address |
|---|---|
| nuc4 | 192.168.4.8 |
| nuc3 | 192.168.4.7 |
| m6 | 192.168.4.14 |
| nynuc | 192.168.5.236 |

Which node owns a playback is not something you choose, so read it back
rather than assuming:

```bash
# On any node — the state machine is replicated, so any node can answer.
cp /srv/plurx/hiqlite/state_machine/db/plurx.db /tmp/ro.db
python3 - <<'PY'
import sqlite3, json, datetime
c = sqlite3.connect("file:/tmp/ro.db?mode=ro", uri=True)
q = """select session_id, state, updated_at_ms, owner_node_id, response_json
       from media_sessions order by updated_at_ms desc limit 5"""
for sid, state, ts, owner, resp in c.execute(q):
    d = json.loads(resp or "{}")
    print(datetime.datetime.fromtimestamp(ts / 1000, datetime.UTC).strftime("%H:%M:%S"),
          state, owner[:8], d.get("delivered_dynamic_range"),
          "dv_profile=%s" % d.get("delivered_dolby_vision_profile"))
PY
# Map owner_node_id to a hostname:
#   select node_id, hostname from cluster_node_hostnames
```

### 2.2 The title needs a fragment index

The conversion happens on the **VOD path**, and only the VOD path writes to
`cache/renditions/`. A title whose fragment index has not been built yet
falls through to live-HLS recovery, which produces a perfectly watchable
stream and no rendition entry at all:

```
WARN VOD prerequisite unavailable; using temporary live-HLS recovery
     file_id=70 refusal="vod_index_pending"
```

Seeing that line means the check cannot run for that title yet, no matter
which client you used. Wait for the index and play it again.

### 2.3 The client must ask to preserve Dolby Vision

The conversion fires for a client that takes profile 8 and not 7 — that is
the gap it exists to close, and `dolby_vision_converts_to_p81` states it as
three conditions: the source is Profile 7 over an HDR10 base, the client
enumerates 8 but not 7, and the build can convert (`PLURX_DV_CONVERT`,
which defaults on).

**An earlier revision of this document named "Safari or the Apple TV app" as
interchangeable triggers. That was wrong, and it cost a run.** Observed
2026-09-02: Apple TV was handed `hdr10`, because it did not ask to preserve
Dolby Vision at all. What a given client actually asks for is a fact to read
out of the decision log, not to assume:

```bash
docker logs plurxd --since 30m 2>&1 \
  | grep "playback capability decision" | tail -5
```

`preserve_dolby_vision=true` on the title you played is the precondition.
Anything else and there is no conversion to measure.

### 2.4 The daemon must actually carry the conversion into the session

Fixed in **#842**, merged 2026-09-03. Before it, the conversion had never run
in production once — `dv_conversions` was empty on all four nodes.

A client cannot ask for a conversion (`CreateSession` has no such field), so
`SessionKind::Copy` is built with `convert_dolby_vision: false` and
`apply_plan_review` is the only assignment that ever sets it true. The
legacy-trusted arm returned no review at all, so for any build sending no
caps document that assignment never ran. The client's *silence* on a field it
has no way to speak about overrode the server's own decision:

```
23:16:21 decision  preserve_dolby_vision=true
         reasons=["Dolby Vision Profile 7 converted to Profile 8.1 for
                   this device; ..."]
23:16:21 WARN create trusted the client's plan echo: this build sends no
              caps document
23:16:34 session  delivered_dolby_vision_profile=7
```

The tell is a session whose `delivered_dolby_vision_profile` is **7**.
`delivered_dolby_vision_profile` returns `Some(8)` unconditionally when the
conversion is on, so a 7 on a title the decision said it would convert means
the flag was dropped between the decision and the session. On a build with
#842 that cannot happen; on an older one, that is what you are looking at.

Raw Profile 7 is the one delivery nothing plays — dual-layer, which no
consumer decoder outside Blu-ray hardware takes. Safari answered
`stream_rejected ... browser refused the remux stream`, and the fallback
tonemapped a 4K Dolby Vision title to SDR H.264, which then failed to load.

---

## 3. What to run

With §2 satisfied, on the **owning node**:

```bash
~/dv-timeline-check                       # default source: Nosferatu (2024)
~/dv-timeline-check "/path/to/other.mkv"  # any other Profile 7 remux
```

The runner finds the newest converting rendition itself, assembles
`init.mp4` plus its segments in playlist order, and runs the check. It exits
2 with the cache listing when there is no converting rendition, which is the
"§2 is not satisfied yet" answer rather than a failure.

Both `~/dv-timeline-check` and `~/verify-dv-timeline` live in the operator's
home directory, not in a checkout — a node's checkout is usually far behind
`main` and will not have the script at all. Copy them from a current tree:

```bash
scp scripts/verify-dv-timeline <node>:~/verify-dv-timeline
scp scripts/dv-timeline-check  <node>:~/dv-timeline-check
ssh <node> 'chmod +x ~/verify-dv-timeline ~/dv-timeline-check'
```

A converting rendition is identifiable on disk, if you want to check by hand:
its `identity.json` carries a `promotion.dolby_vision` object. Every other
rendition has `"dolby_vision": null` or no such key.

### How to read it

Exit 0 is a pass. Three lines print before the verdict, and each is worth
reading even on a pass:

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

## 4. Run it seeked, not only from zero

A session that starts at zero was correct even under the withdrawn design.
plurx's VOD producer respawns at plan boundaries mid-film, so **seeks are the
normal case**, not the exception. Repeat the capture from at least two
mid-film positions — a plan boundary and something a few minutes past one —
and run the script against each.

---

## 5. The other three things worth checking while the media is in hand

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

## 6. What a failure means

If the timeline check fails, **do not tune anything**. The post-mux design's
whole claim is that timestamps are copied and never reconstructed, so there is
no constant to fit and nothing to anchor. A failure means something is wrong
with that claim, and the useful next step is the script's own output — which
of the three lines failed, and whether it failed only on seeked runs.

This project has already had one finding retracted for being measured against
a control that never runs in production. A number that disagrees with the
design is a reason to re-examine the design, not to correct the number.

And a check that reports nothing is not a pass. Four consecutive "no
converting rendition" results read as a broken script for most of an evening;
they were in fact §2.1, §2.2 and §2.4 in a row, the last of them a shipped
bug that had disabled the entire feature. **The empty answer is a
prerequisite failure until §2 proves otherwise.**
